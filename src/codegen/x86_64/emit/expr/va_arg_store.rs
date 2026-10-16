//! emit/expr/va_arg_store.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::BoolExpect;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
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

/// `va_list` 结构体四字段的**落点**（寻址基）。
///
/// 与 `VaListAddress`（回答“寄存器还是内存里的指针”）不同轴：本枚举回答的是
/// “这个 `va_list` 对象本身住在哪种内存里”，因而它的 `gp_offset` /
/// `fp_offset` / `overflow_arg_area` / `reg_save_area` 该怎么读写。三种落点必须
/// 共用同一套 ABI 判定，否则会出现“一侧写真全局内存、另一侧读函数私有
/// 懒分配栈槽”这类落点不一致。
#[derive(Clone, Copy)]
enum VaListLoc {
    /// 命名局部 `va_list`：结构体在 `[r13 + 字段偏移]`（字段基址 = 给定槽偏移）。
    Slot(i32),
    /// 全局 `va_list`：调用点先 `lea r10, [rip+name]`，字段在 `[r10 + ...]`。
    R10,
    /// 已退化为指针的 `va_list`：`rcx` 持指针，字段在 `[rcx + ...]`。
    Rcx,
}

impl Codegen {
    /// va_arg 的结果在 dcc 值模型里是否**以缓冲区地址交付**（而非寄存器值）。
    ///
    /// 这条轴与“字节从哪个区读”（`abi_classes` / `classes_force_mem`）正交：
    /// 前者回答交付约定，后者回答字节来源。旧代码在分发点直接写
    /// `matches!(Struct|Union) && size > 8`，把两轴混成“类型的书写形态”，于是：
    /// - 裸 16 字节向量（`__m128d` = 分类 [Sse, SseUp]）被判为标量，从单
    ///   个 xmm 寄存器交付，而它的值需两个 8 字节块；
    /// - 含多 eightbyte 成员的聚合经 va_list 取参时走不到按分类装载路径。
    ///
    /// 例外是 dcc 特有的**值模型**约定（不是 ABI）：整个对象能被一个寄存器装
    /// 下的纯整数聚合（size <= 8 且分类恰为 `[Integer]`，如
    /// `struct { int a; int b; }` —— 两个 int 落在**同一个** eightbyte）可以按
    /// 寄存器值交付；强行改成缓冲区地址会让通用路径把值当指针解引用。
    pub(crate) fn va_arg_result_in_memory(ty: &Type) -> bool {
        if Self::type_has_vla(ty) {
            return true;
        }
        // `_Complex` 恒以缓冲区交付：它的**值模型**是「内存对 (+ SSE 内型时
        // xmm0=实部)」，永远不可能是单寄存器标量。注意这与“字节从哪个区读”
        // 那条轴（`abi_classes`）正交：放开 float/double 复数的分类后，其变参
        // 字节来自 FP 保存区的 1~2 个 16 字节槽（`[Sse]` / `[Sse,Sse]`），必须
        // 由按分类逐 eightbyte 搬运的聚合路径装载；单块 xmm 路径
        // (`gen_va_arg_fp`) 只能交付一个寄存器，装不下两分量。
        if matches!(ty, Type::Complex(..)) {
            return true;
        }
        if !matches!(ty, Type::Struct(..) | Type::Union(..) | Type::Array(..) | Type::Vector(..)) {
            return false;
        }
        !(ty.size() <= 8 && matches!(Self::abi_classes(ty).as_slice(), [AbiClass::Integer]))
    }

    /// 缓冲区交付的 `va_arg` 是否**不**占用 `alloc_sret_slot`，而是直接落在
    /// 帧底固定的复数物化带 [`Codegen::complex_result_off`]（即上面那条
    /// “`_Complex` 恒以缓冲区交付”的分支）。
    ///
    /// 帧静态扫描 (`sret_bytes_expr`) 与实际装载代码必须对“本表达式到底分不分
    /// 槽”同源，否则扫描会为不复用的槽推高水位、真正复用的槽却算轻。
    pub(crate) fn va_arg_result_in_cx_band(ty: &Type) -> bool {
        matches!(ty, Type::Complex(..))
    }

    /// eightbyte 分类表 → 本对象实际占用的 `(GP 寄存器数, XMM 寄存器数)`。
    ///
    /// 这两个数是 SysV「寄存器保存区够不够用」判据（`gp_offset > 48 - 8*n_gp`
    /// 与 `fp_offset > 176 - 16*n_xmm`）与游标推进步长的输入。GP 侧一个 eightbyte
    /// 就是一个 8 字节槽（`__int128` = 2 个 eightbyte = 16 字节），数 eightbyte
    /// 与数寄存器一致；**XMM 侧不一致**：`SseUp` 是前一个 `Sse` 所在 16 字节槽的
    /// 高半延续，不另开槽。
    ///
    /// GCC 实测（`-O0` 看 `function_arg_advance` 生成的槽位）：
    /// - `__m128d` = `[Sse, SseUp]` → 只用 `xmm0`（一个槽）；
    /// - `struct {double a; double b;}` = `[Sse, Sse]` → 用 `xmm0` + `xmm1`（两个槽）。
    ///
    /// 旧实现按 SSE 家族 eightbyte 个数算，`[Sse, SseUp]` 被当成 2 个寄存器，上限
    /// 从 `176-16=160` 缩到 `176-32=144`，于是第 8 个向量参数（`fp_offset=160`）
    /// 被误判为溢出、改从 `overflow_arg_area` 取，而 caller 把它留在了 `xmm7` 槽里。
    fn va_arg_reg_counts(classes: &[AbiClass]) -> (i32, i32) {
        let n_gp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Integer))
            .count() as i32;
        let n_xmm = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Sse))
            .count() as i32;
        (n_gp, n_xmm)
    }

    /// 第 `c` 个 eightbyte 在 FP 保存区里相对 `fp_offset` 起点的**字节偏移**。
    ///
    /// 分组规则同 [`Self::va_arg_reg_counts`]：`Sse` 开一个新的 16 字节槽取低半，
    /// `SseUp` 续用上一个 `Sse` 的槽取高半（+8）。旧实现用“SSE 家族个数的奇
    /// 偶”近似：对 `[Sse, SseUp]` 恰好对，对 `[Sse, Sse]` 则让第二块从**同一槽
    /// 的 +8** 读（应从下一个槽的 +0），游标也只推 16（应推 32），与 GCC 不同源。
    fn va_arg_sse_disp(classes: &[AbiClass], c: usize) -> i32 {
        let n_sse_before = classes[..c].iter().filter(|k| **k == AbiClass::Sse).count() as i32;
        match classes[c] {
            // 孤立的 SseUp（分类器不会产出）降级到当前槽高半，避免算出负偏移。
            AbiClass::SseUp => (n_sse_before - 1).max(0) * 16 + 8,
            _ => n_sse_before * 16,
        }
    }

    /// FP 槽的读出指令按**对象宽度**归一化：16→movups、4→movss、其余→movsd。
    ///
    /// 旧实现对 `Float` 写 `movss`、其余一律 `movsd`，于是 16 字节的
    /// `__m128d` 只能取到低 8 字节。`base_index` 选基址形态：保存区需加
    /// `fp_offset`（`(%rcx,%rax)`），overflow 游标已在指针里（`(%rcx)`）。
    fn emit_va_sse_load(&mut self, sz: usize, base_index: bool) {
        let width = match sz {
            16 => crate::codegen::x86_64::opcode::SimdWidth::PS, // movups
            4 => crate::codegen::x86_64::opcode::SimdWidth::SS,  // movss
            _ => crate::codegen::x86_64::opcode::SimdWidth::SD,  // movsd
        };
        let mem = if base_index {
            MemOperand {
                disp: 0,
                base: Some(1),
                index: Some((0, 1)),
                seg: None,
            }
        } else {
            MemOperand {
                disp: 0,
                base: Some(1),
                index: None,
                seg: None,
            }
        };
        self.emit_insn(&Insn::new(
            X86_64Opcode::SimdMov {
                width,
                opcode10: true,
                dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
                src: Operand::Mem(mem),
            },
            &[],
        ));
    }

    /// 寄存器保存区路径的公共装载体：按 ABI 分类逐 eightbyte 把值搬进 `rdi`
    /// 指向的对象。三个调用点（named / ptr / global `va_list`）此前各持一份
    /// 逐字副本，任何单点修复都会另外两处静默失效，现收敛到此处。
    ///
    /// 寄存器约定：`rcx` = reg_save_area、`rax` = gp_offset、`rdx` = fp_offset。
    /// 本函数只推进 `rax`/`rdx` 不回写内存——回写位置随 `va_list` 基址而异
    /// （`[r13+ap_off]` / `[r11]` / `[r10]`），留给调用点完成。
    fn emit_va_arg_reg_save_load(&mut self, classes: &[AbiClass]) {
        for (c, cls) in classes.iter().enumerate() {
            let off8 = (c as i32) * 8;
            match cls {
                AbiClass::Integer => {
                    // 一个 GP eightbyte = 一个 8 字节槽, 逐块读 + 推进。
                    // mov r9, (%rcx,%rax): 4C 8B 0C 01 (SIB scale=0 idx=rax=0 base=rcx=1)
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                9,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Mem(
                                crate::codegen::x86_64::opcode::MemOperand {
                                    disp: 0,
                                    base: Some(1),
                                    index: Some((0, 1)),
                                    seg: None,
                                },
                            ),
                        },
                        &[],
                    )); // mov r9, (%rcx,%rax)
                    if off8 == 0 {
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            9,
                            7,
                            0,
                        )); // mov [rdi], r9
                    } else if off8 < 128 {
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            9,
                            7,
                            off8,
                        )); // mov [rdi+disp8], r9
                    } else {
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            9,
                            7,
                            off8,
                        )); // mov [rdi+disp32], r9
                    }
                    self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 0, 8)); // add $8, %rax
                },
                AbiClass::Sse | AbiClass::SseUp => {
                    // 槽内位置由分组规则算出(见 `va_arg_sse_disp`), 不能按奇偶近似。
                    let disp = Self::va_arg_sse_disp(classes, c);
                    // movsd xmm0, disp(%rcx,%rdx): SIB scale=1, idx=rdx(2), base=rcx(1).
                    self.emit_insn(&Insn::new(
                        X86_64Opcode::SimdMov {
                            width: crate::codegen::x86_64::opcode::SimdWidth::SD,
                            opcode10: true,
                            dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B128),
                            src: Operand::Mem(MemOperand {
                                disp,
                                base: Some(1),
                                index: Some((2, 1)),
                                seg: None,
                            }),
                        },
                        &[],
                    ));
                    if off8 == 0 {
                        self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xD6, 0x07]));
                    // movq [rdi], xmm0
                    } else if off8 < 128 {
                        self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xD6, 0x47])); // movq [rdi+disp8], xmm0
                        self.emit_insn(&Insn::raw_bytes(&[off8 as u8]));
                    } else {
                        self.emit_insn(&Insn::raw_bytes(&[0x66, 0x0F, 0xD6, 0x87])); // movq [rdi+disp32], xmm0
                        self.emit_insn(&Insn::raw_bytes(&off8.to_le_bytes()));
                    }
                },
                _ => {},
            }
        }
        // FP 游标按**实际占用的寄存器个数**一次性推进 (每寄存器 16 字节),
        // 不再在循环里按奇偶逐次 `add rdx,16`。
        let n_xmm = Self::va_arg_reg_counts(classes).1;
        if n_xmm > 0 {
            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 2, (n_xmm * 16) as i64));
            // add rdx, imm32
        }
    }
    fn emit_va_arg_int_load(&mut self, sz: usize, signed: bool, rex: u8, modrm: &[u8]) {
        use crate::codegen::shared::asm_operand::Width;
        // 统一映射成 typed MovExt / MovRm mem-load, SIB (base+rax*1) 或纯 base.
        // 手写历史约定: dst=rax(0); base 由 rex 决定 (rcx=1 无REX, r11=11 带REX.B);
        // index=rax(0) 时 modrm 是 [0x04, sib=0x01 or 0x03]; 无 index 时 modrm=[rm_byte].
        let base: u8 = if rex & 0x01 != 0 { 11 } else { 1 };
        let use_sib_index_rax = modrm.first() == Some(&0x04);
        let (from, to) = match sz {
            1 => (Width::B8, Width::B32),
            2 => (Width::B16, Width::B32),
            4 if signed => (Width::B32, Width::B64),
            4 if !signed => {
                // mov eax, [base]: 48/REX.B + 8B 不是 MovExt; 走 MovRm load B32.
                if use_sib_index_rax {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: Width::B32,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(0, Width::B32),
                            src: crate::codegen::x86_64::opcode::Operand::Mem(
                                crate::codegen::x86_64::opcode::MemOperand {
                                    disp: 0,
                                    base: Some(base),
                                    index: Some((0, 1)),
                                    seg: None,
                                },
                            ),
                        },
                        &[],
                    ));
                } else {
                    self.emit_insn(&Insn::mov_rm_mem(true, Width::B32, 0, base, 0));
                }
                return;
            },
            _ => {
                // mov rax, [base+rax*1] 或 mov rax, [base] — 走 MovRm B64.
                if use_sib_index_rax {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(0, Width::B64),
                            src: crate::codegen::x86_64::opcode::Operand::Mem(
                                crate::codegen::x86_64::opcode::MemOperand {
                                    disp: 0,
                                    base: Some(base),
                                    index: Some((0, 1)),
                                    seg: None,
                                },
                            ),
                        },
                        &[],
                    ));
                } else {
                    self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 0, base, 0));
                }
                return;
            },
        };
        if use_sib_index_rax {
            self.emit_insn(&Insn::mov_ext_mem_sib(signed, from, to, 0, base, 0, 1));
        } else {
            self.emit_insn(&Insn::mov_ext_mem(signed, from, to, 0, base, 0));
        }
    }
    pub(crate) fn emit_va_start(&mut self, ap_name: &str) {
        if self.is_global_not_local(ap_name) {
            // 全局 va_list: rip 相对写真全局内存, 与 gen_va_arg_global
            // 的读取路径一致 (跨函数可见, 如 stdarg-1 f2: va_start(gap)
            // 后 bar 内 va_arg(gap,...))。懒分配栈槽是函数私有的, 读写
            // 跨函数不一致。
            self.emit_global_addr(10, ap_name); // r10 = &gap
            self.emit_va_start_at(10);
            return;
        }
        // 局部: 按 va_list 真实大小 (24B=3 槽) 注册栈槽,
        // 避免懒分配 1 槽时四字段写入覆盖相邻变量
        let ap_off = self.register_var(ap_name, &Self::va_list_slot_ty());
        let gp_init = self.variadic_named_gp.min(6) * 8;
        // SSE 命名参数各占 reg_save_area 中 16 字节槽
        let fp_init = 48 + self.variadic_named_fp * 16;
        self.store_stack_imm32(ap_off, gp_init);
        self.store_stack_imm32(ap_off + 4, fp_init);
        // overflow_arg_area: r13 是 push 后的 rsp,
        //   [r13+0]=saved r13, [r13+8]=ret addr, [r13+16]=caller 栈上第 1 个变参.
        // 与 frame_size 无关 — 帧是 sub 出的, 与 caller 栈方向相反.
        let overflow_off: i64 = self.variadic_overflow_offset;
        self.emit_lea_r13(0, overflow_off);
        self.store_stack(0, ap_off + 8);
        self.emit_lea_r13(0, self.variadic_reg_save_offset);
        self.store_stack(0, ap_off + 16);
    }
    pub(crate) fn emit_va_start_at(&mut self, base: u8) {
        let gp_init = self.variadic_named_gp.min(6) * 8;
        let fp_init = 48 + self.variadic_named_fp * 16;
        self.emit_store_imm32_mem(base, 0, gp_init);
        self.emit_store_imm32_mem(base, 4, fp_init);
        self.emit_lea_r13(0, self.variadic_overflow_offset);
        self.emit_store_reg_mem(base, 8, 0);
        self.emit_lea_r13(0, self.variadic_reg_save_offset);
        self.emit_store_reg_mem(base, 16, 0);
    }
    pub(crate) fn gen_va_arg(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        // 路由到按 16 字节 FP 槽取数的单块路径。判据用 ABI 分类而非类型书写
        // 形态（`float`/`double` 与单 eightbyte SSE 对象同走此路）。
        //
        // 限定 `size <= 8` 是必要的：dcc 内部的**向量值模型是 GP-block**，
        // `va_arg(ap, __m128d)` 的结果需以两个 8 字节 GP 块交付，而本函数把值
        // 送进单个 xmm 寄存器。“字节来源是 FP 槽”与“值表示是 GP block”是两条
        // 正交轴，合并它们会得到错误结果（实测：路由放开到 16 字节后
        // `sseup_shares_one_slot` 仍错，两个向量都读错）。
        // 16 字节 SSE 对象不走本函数：由 `gen_expr_va_arg` 用
        // `va_arg_result_in_memory` 判为“以缓冲区地址交付”，改走
        // `gen_va_arg_struct` / `gen_va_arg_ptr_struct` 的按分类逐块装载路径
        // （消费侧则 destination-direct 写入目标，见 `gen_aggregate_assign`）。
        if self.target.sse
            && arg_ty.size() <= 8
            && Self::classes_all_sse_family(&Self::abi_classes(arg_ty))
        {
            self.gen_va_arg_fp(ap_name, arg_ty, dst);
            return;
        }
        // Long double: SysV MEMORY 类，始终在栈上传递 (overflow_arg_area)
        // 16 字节，不经过寄存器保存区
        if matches!(arg_ty, Type::LongDouble) {
            self.gen_va_arg_long_double(ap_name, dst);
            return;
        }
        // __int128 / unsigned __int128: 占两个 GP eightbyte 的标量。
        // GCC 实测判据 (`cmpl $39, gp_offset; ja 内存路径`)：高半必须仍落在 GP 保存区
        // 最后一个槽内 (gp_offset + 8 <= 47)。装不下则：
        //   1. **不拆分** —— 整个对象从 overflow_arg_area 取（旧实现的 hybrid 路径
        //      把低半留在寄存器保存区 [40]、高半从溢出区取，与 GCC 不同源）；
        //   2. **gp_offset 不推进**（旧实现连内存路径也 `+= 16`，把游标双推进）；
        //   3. 溢出游标先向上对齐到 16 再读，读完推 16。
        // 实测：`gp_offset == 40` 时读 `va_arg(ap, __int128)` 与 GCC 不同源（rc=3）。
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            // 落点判据必须在本分支内部同样成立：此前它直用 `var_offset(ap_name)`
            // + r13 相对寻址，于是全局 `va_list` 读的是本函数私有的懒分配栈槽
            // (未初始化) —— `va_start` 却写真全局内存，两侧落点不一致。
            if self.is_global_not_local(ap_name) {
                self.gen_va_arg_int128_global(ap_name);
                return;
            }
            let ap_off = self.var_offset(ap_name) as i32;
            self.gen_va_arg_int128_at(VaListLoc::Slot(ap_off));
            return;
        }

        // 全局 va_list: 无栈槽，直接 rip 相对访问全局结构 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_global(ap_name, arg_ty, dst);
            return;
        }
        let ap_off = self.var_offset(ap_name) as i32;
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = gp_offset
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B32,
            0,
            13,
            ap_off,
        ));
        self.emit_insn(&Insn::alu_rm_reg_imm8(7, 0, 0x30)); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + gp_offset
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 16,
        ));
        let sz = arg_ty.size();
        let signed = Self::va_arg_ty_signed(arg_ty);
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x40, &[0x04, 0x01]),
            2 => self.emit_va_arg_int_load(2, signed, 0x40, &[0x04, 0x01]),
            4 => self.emit_va_arg_int_load(4, signed, 0x40, &[0x04, 0x01]),
            _ => {
                // mov rax, (%rcx,%rax): 48 8B 04 01 (SIB scale=0 idx=rax=0 base=rcx=1)
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                            crate::codegen::x86_64::opcode::MemOperand {
                                disp: 0,
                                base: Some(1),
                                index: Some((0, 1)),
                                seg: None,
                            },
                        ),
                    },
                    &[],
                ));
            },
        }
        self.emit_insn(&Insn::alu_mem_imm8_b32(0, 13, ap_off, step as i32));
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 8,
        ));
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x40, &[0x01]),
            2 => self.emit_va_arg_int_load(2, signed, 0x40, &[0x01]),
            4 => self.emit_va_arg_int_load(4, signed, 0x40, &[0x01]),
            _ => {
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                            crate::codegen::x86_64::opcode::MemOperand {
                                disp: 0,
                                base: Some(1),
                                index: None,
                                seg: None,
                            },
                        ),
                    },
                    &[],
                ))
                .expect("typed-opcode: new");
            },
        }
        self.push(0);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 8,
        ));
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 1, step as i64)); // add rcx, imm8
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 8,
        ));
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    fn gen_va_arg_fp(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        // 全局 va_list: 直接 rip 相对访问 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_fp_global(ap_name, arg_ty, dst);
            return;
        }
        let ap_off = self.var_offset(ap_name) as i32;
        let sz = arg_ty.size();
        let reg_step = 16i32; // reg_save_area 中 XMM 槽为 16 字节对齐
                              // overflow_arg_area 按对象自身向上取整到 8 字节：float/double 走 8，
                              // `__m128` 系走 16。写死 8 会让 16 字节向量的后续参数错位。
        let overflow_step = sz.div_ceil(8).max(1) as i32 * 8;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = fp_offset (va_list + 4)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B32,
            0,
            13,
            ap_off + 4,
        ));
        // cmp $176, %eax — 176 超出 imm8 范围，须用 32 位立即数
        self.emit_insn(&Insn::cmp_eax_imm32(176));
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + fp_offset
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 16,
        ));
        self.emit_va_sse_load(sz, true);
        self.emit_insn(&Insn::alu_mem_imm8_b32(0, 13, ap_off + 4, reg_step));
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 8,
        ));
        self.emit_va_sse_load(sz, false);
        self.push(0);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 8,
        ));
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 1, overflow_step as i64)); // add rcx, imm8
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            13,
            ap_off + 8,
        ));
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    fn gen_va_arg_fp_global(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        let sz = arg_ty.size();
        let overflow_step = sz.div_ceil(8).max(1) as i32 * 8;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B32,
            0,
            10,
            4,
        )); // mov eax, [r10+4] (fp_offset)
        self.emit_insn(&Insn::cmp_eax_imm32(176)); // cmp $176, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            16,
        )); // mov rcx, [r10+16]
        self.emit_va_sse_load(sz, true);
        self.emit_insn(&Insn::alu_mem_imm8_b32(0, 10, 4, 16)); // add dword [r10+4], 16
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        )); // mov rcx, [r10+8]
        self.emit_va_sse_load(sz, false);
        self.push(0);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        ));
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 1, overflow_step as i64)); // add rcx, imm8
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        )); // mov [r10+8], rcx
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    /// `__int128` 装载体的单一实现：判定与指令完全一致，只有 va_list 四个
    /// 字段的**寻址**随落点而变。三种落点原先各有一份（named 版还带一条与 GCC
    /// 不同源的 hybrid 拆分路径、全局版的 r10 未设置且溢出分支是死代码、
    /// 指针版根本没有 Int128 分支），收敛到此。
    ///
    /// SysV / GCC 真值（由 `cmpl $39, gp_offset; ja 内存路径` 实测得出）：
    /// - `gp_offset <= 39`（高半仍落在 GP 区最后一个槽 40..47 内）→ 两半都在
    ///   寄存器保存区，`gp_offset += 16`；
    /// - 否则**不拆分**：整个对象从 16 字节对齐后的 `overflow_arg_area` 取，
    ///   `gp_offset` **不推进**，溢出游标 += 16。
    ///
    /// 字段偏移：0 = gp_offset(u32)、8 = overflow_arg_area、16 = reg_save_area。
    /// 基址寄存器在本函数内必须不可变（reg_save_area 存于 rdx，不复写基址）。
    fn gen_va_arg_int128_at(&mut self, loc: VaListLoc) {
        let mem = self.fresh_label();
        let done = self.fresh_label();

        // eax = gp_offset (必须 32 位读: 高 4 字节是 fp_offset, 按 64 位读会把它灌进高位)
        match loc {
            VaListLoc::Slot(off) => {
                // mov eax, [r13+disp32]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B32,
                    0,
                    13,
                    off,
                ));
            },
            VaListLoc::R10 => {
                // mov eax, [r10]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B32,
                    0,
                    10,
                    0,
                ));
            },
            VaListLoc::Rcx => {
                // mov eax, [rcx]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B32,
                    0,
                    1,
                    0,
                ));
            },
        }
        self.emit_insn(&Insn::alu_rm_reg_imm8(7, 0, 0x27)); // cmp $39, %eax
        self.jmp_rel32(&mem, JumpKind::Ja);

        // ---- 寄存器保存区路径 ----
        // rdx = reg_save_area; rax = rdx + gp_offset; gp_offset += 16
        match loc {
            VaListLoc::Slot(off) => {
                // mov rdx, [r13+disp32]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    2,
                    13,
                    off + 16,
                ));
            },
            VaListLoc::R10 => {
                // mov rdx, [r10+16]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    2,
                    10,
                    16,
                ));
            },
            VaListLoc::Rcx => {
                // mov rdx, [rcx+16]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    2,
                    1,
                    16,
                ));
            },
        }
        // P11 Bucket P: lea rax, (%rdx,%rax) → typed Lea SIB.
        self.emit_insn(&Insn::new(
            X86_64Opcode::Lea {
                dst: Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                mem: MemOperand {
                    disp: 0,
                    base: Some(2),
                    index: Some((0, 1)),
                    seg: None,
                },
                rip_relative: false,
            },
            &[],
        ));
        match loc {
            VaListLoc::Slot(off) => {
                self.emit_insn(&Insn::alu_mem_imm8_b32(0, 13, off, 16)); // add dword [r13+disp32], 16
            },
            VaListLoc::R10 => {
                self.emit_insn(&Insn::alu_mem_imm8_b32(0, 10, 0, 16));
            }, // add dword [r10], 16
            VaListLoc::Rcx => {
                self.emit_insn(&Insn::alu_mem_imm8_b32(0, 1, 0, 16));
            }, // add dword [rcx], 16
        }
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 内存路径: rax = 对齐后的溢出区地址, 游标 += 16 (gp_offset 不动) ----
        self.label(&mem);
        match loc {
            VaListLoc::Slot(off) => {
                // mov rax, [r13+disp32]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    13,
                    off + 8,
                ));
            },
            VaListLoc::R10 => {
                // mov rax, [r10+8]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    10,
                    8,
                ));
            },
            VaListLoc::Rcx => {
                // mov rax, [rcx+8]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    1,
                    8,
                ));
            },
        }
        // SysV 3.5.7: 当 gp_offset > 39 时, 整个 __int128 从 overflow_arg_area 取.
        // caller 在 call 前 push 目标地址后, rsp 落入 (8 mod 16). 即使 caller 侧
        // 做了 +8 静态 padding (见 abi/call.rs), 这个 +8 是 frame_bytes 的一部分,
        // 从 callee 视角看 overflow_arg_area = r13 + 16, 该地址本身已是 16 对齐
        // (因为 call 指令 push ret addr 前 rsp 是 16 对齐, push 后 r13 = rsp - 8,
        // r13 + 16 = 16 对齐地址 + 8 = 8 mod 16). 所以 va_arg 必须把 overflow
        // 地址舍入到 16 对齐再读.
        // 注意: 不能把 rax (刚 load 的 overflow_arg_area 指针) 再 store 回
        // va_list 的 overflow_arg_area 字段 — 那是在原地覆写同一个槽,
        // 等于把指针写回它自己刚 load 出来的值 (无意义), 且会破坏后续
        // va_arg 的游标推进 (pr92904 f1 路径 segfault: 读到垃圾地址).
        // 正确做法: rax = (overflow + 15) & ~15 (16 对齐), 然后推进游标 += 16.
        // add rax, 15
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 0, 15));
        // and rax, -16
        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 0, -16i64));
        // 推进 overflow_arg_area 游标 += 16 (原地在 va_list 字段上加, 不覆写 rax)
        match loc {
            VaListLoc::Slot(off) => {
                // add qword [r13+disp32], 16
                self.emit_insn(&Insn::alu_mem_imm32_b64(0, 13, off + 8, 16));
            },
            VaListLoc::R10 => {
                // add qword [r10+8], 16
                self.emit_insn(&Insn::alu_mem_imm32_b64(0, 10, 8, 16));
            },
            VaListLoc::Rcx => {
                // add qword [rcx+8], 16
                self.emit_insn(&Insn::alu_mem_imm32_b64(0, 1, 8, 16));
            },
        }

        // ---- done: __int128 固定以 rdx:rax 交付 (SysV) ----
        self.label(&done);
        // mov rdx, 8(%rax)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            2,
            0,
            8,
        ));
        // mov rax, (%rax)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            0,
            0,
            0,
        ));
    }

    /// 全局 `va_list` + `__int128`：基址由 `lea r10, [rip+name]` 固定后交给
    /// 共用实现。
    fn gen_va_arg_int128_global(&mut self, ap_name: &str) {
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.gen_va_arg_int128_at(VaListLoc::R10);
    }
    fn gen_va_arg_global(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();

        // __int128 / unsigned __int128: 与本地版同一判据, 委派给专用的
        // 全局变体。旧副本有三处硬错：它在 `emit_global_addr(10, ..)` **之前**
        // 就进入，所以 `emit_va_arg_int128_from_gp` 解引用的是未设置的 r10；
        // `label(&overflow)` 无人跳入 (整个溢出分支是死代码，且带一个无配对的
        // `pop(0)`)；而且它无条件 `add dword [r10], 16`，没有“装不下就不推进”判定。
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            self.gen_va_arg_int128_global(ap_name);
            return;
        }

        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
                                            // mov eax, [r10] (gp_offset)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B32,
            0,
            10,
            0,
        ));
        self.emit_insn(&Insn::alu_rm_reg_imm8(7, 0, 0x30)); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // 寄存器保存区路径：rcx = reg_save_area
        // mov rcx, [r10+16]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            16,
        ));
        let gsz = arg_ty.size();
        let gsigned = Self::va_arg_ty_signed(arg_ty);
        match gsz {
            1 => self.emit_va_arg_int_load(1, gsigned, 0x40, &[0x04, 0x01]),
            2 => self.emit_va_arg_int_load(2, gsigned, 0x40, &[0x04, 0x01]),
            4 => self.emit_va_arg_int_load(4, gsigned, 0x40, &[0x04, 0x01]),
            _ => {
                // mov rax, (%rcx,%rax): 48 8B 04 01 (SIB scale=0 idx=rax=0 base=rcx=1)
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                            crate::codegen::x86_64::opcode::MemOperand {
                                disp: 0,
                                base: Some(1),
                                index: Some((0, 1)),
                                seg: None,
                            },
                        ),
                    },
                    &[],
                ));
            },
        }
        self.emit_insn(&Insn::alu_mem_imm8_b32(0, 10, 0, 8)); // add dword [r10], 8
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        // mov rcx, [r10+8]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        ));
        match gsz {
            1 => self.emit_va_arg_int_load(1, gsigned, 0x40, &[0x01]),
            2 => self.emit_va_arg_int_load(2, gsigned, 0x40, &[0x01]),
            4 => self.emit_va_arg_int_load(4, gsigned, 0x40, &[0x01]),
            _ => {
                // mov rax, [rcx]
                self.emit_insn(&Insn::mov_rm_mem(
                    true,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    1,
                    0,
                ));
            },
        }
        self.push(0);
        // mov rcx, [r10+8]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        ));
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 1, step as i64)); // add rcx, imm8
                                                                       // mov [r10+8], rcx
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        ));
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    /// D9: 把 `long double` 变参槽 load 到 st(0)。
    ///
    /// SysV AMD64 下 `long double` 属 **X87 类**：变参实参永远放在 16 字节对齐的
    /// `overflow_arg_area`，内容是调用方 `fstpt` 写出的**真 80-bit 编码**（低 10
    /// 字节有效，`+10..16` 是实现定义 padding）。旧代码发 `movsd (%r9), %xmm0`
    /// 只取低 8 字节当成 f64 位型 —— 实际读到的是那 64 位**尾数**，值完全无关
    /// （实测 `(long long)a` 得 LLONG_MIN）。
    ///
    /// `long double` 现在内部也是**真 80-bit 对象 + st(0) 值通道**
    /// （`float_in_st0` 对 `LongDouble` 恒为真，二元算术走 `gen_float_binop` 的
    /// x87 通路，落槽用 `fstpt`），所以此处**不需要任何转换**：一条 `fld tbyte`
    /// 把槽里的 80-bit 压到 st(0)，就是消费端期望的形式。
    ///
    /// 旧实现发的是 `fld tbyte` → `fstp qword`（硬件舍入）→ `movsd`，那是当时
    /// “低 8 字节 = f64”不变量下的**跨界降级垫片**。不变量被取消后，那个
    /// `fstp qword` 成了纯粹的精度损失点（把刚读到的 64 位有效尾数舍回 53 位），
    /// 还额外占用 rsp scratch。删。
    ///
    /// `base_r9` 选基址寄存器：named 变体用 r9，global / ptr 变体用 rcx。
    fn emit_ld80_load_to_st0(&mut self, base_r9: bool) {
        if base_r9 {
            // REX.B=1 使 rm=001→r9; ModRM = 00_101_001 (fld 是 x87 /5)
            self.emit_insn(&Insn::x87_mem(
                crate::codegen::x86_64::opcode::X87Op::FldMem { width: 10 },
                9,
                0,
            )); // fld tbyte (%r9)
        } else {
            self.emit_insn(&Insn::x87_mem(
                crate::codegen::x86_64::opcode::X87Op::FldMem { width: 10 },
                1,
                0,
            )); // fld tbyte (%rcx)
        }
    }
    fn gen_va_arg_long_double(&mut self, ap_name: &str, _dst: u8) {
        // 全局 va_list: 直接 rip 相对访问 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_long_double_global(ap_name);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        // r8 = overflow_arg_area (va_list + 8)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            8,
            13,
            ap_off as i32 + 8,
        )); // mov r8, [r13+ap_off+8]
            // r9 = (r8 + 15) & ~15 (16 字节对齐)
        self.emit_insn(&Insn::lea_reg_base_disp(9, 8, 15)); // lea r9, [r8+15]
                                                            // and r9, ~15: B64 0x81 id form (imm32 = -16 = 0xFFFFFFF0)
        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 9, -16));
        // 从 overflow_arg_area 把真 80-bit 对象直接 load 到 st(0)（D9）。
        self.emit_ld80_load_to_st0(true); // fld tbyte (%r9) → st(0)，无损
                                          // r9 += 16 (推进到下一个 16 字节对齐位置)
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 9, 16)); // add r9, 16
                                                              // 写回 overflow_arg_area: [r13+ap_off+8] = r9
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            9,
            13,
            ap_off as i32 + 8,
        )); // mov [r13+ap_off+8], r9 (disp32)
            // 结果在 st(0)（`long double` 的 rvalue 通道）
    }
    fn gen_va_arg_long_double_global(&mut self, ap_name: &str) {
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
                                            // rcx = overflow_arg_area ([r10+8]): REX.W+B (0x49) 使 rm=010→r10
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        )); // mov rcx, [r10+8]
            // r9 = (rcx + 15) & ~15 (16 字节对齐)
        self.emit_insn(&Insn::lea_reg_base_disp(1, 1, 15)); // lea rcx, [rcx+15]
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op: crate::codegen::x86_64::opcode::AluOp::And,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Imm(-16),
            },
            &[],
        )); // and rcx, ~15
            // 从 overflow_arg_area load 真 80-bit 到 st(0) (D9)。
        self.emit_ld80_load_to_st0(false); // fld tbyte (%rcx) → st(0)，无损
                                           // rcx += 16 (推进到下一个 16 字节对齐位置)
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                op: crate::codegen::x86_64::opcode::AluOp::Add,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Imm(16),
            },
            &[],
        )); // add rcx, 16
            // 写回 [r10+8]: REX.W+B 使 rm=010→r10, reg=001→rcx 源
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            10,
            8,
        )); // mov [r10+8], rcx
            // 结果在 st(0)（`long double` 的 rvalue 通道）
    }
    pub(crate) fn gen_va_arg_ptr(&mut self, arg_ty: &Type, dst: u8) {
        if matches!(arg_ty, Type::Float | Type::Double) && self.target.sse {
            self.gen_va_arg_ptr_fp(arg_ty, dst);
            return;
        }
        // __int128 / unsigned __int128: 本变体以前根本没有这个分支，于是 16 字节的
        // 标量落回单 8 字节通用路径：只取到（并只推进）低半。与 GCC 不同源（实测
        // 指针型 `va_list` 上 `va_arg(ap, __int128)` 及其后一个参数全错）。现与
        // named / global 两变体共用同一判据，基址是 `rcx`（本函数内不得复写）。
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            self.gen_va_arg_int128_at(VaListLoc::Rcx);
            return;
        }
        // Long double: SysV MEMORY 类, 从 overflow_arg_area 读取
        // (值通道是 st(0)，与其他 long double rvalue 一致)
        if matches!(arg_ty, Type::LongDouble) {
            self.emit_insn(&Insn::mov_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                11,
                1,
            )); // mov r11, rcx (回写基址)
            self.emit_insn(&Insn::mov_rm_mem(
                true,
                crate::codegen::shared::asm_operand::Width::B64,
                1,
                1,
                8,
            )); // mov rcx, [rcx+8] (overflow)
                // rcx = (rcx + 15) & ~15 (16 字节对齐)
            self.emit_insn(&Insn::lea_reg_base_disp(1, 1, 15)); // lea rcx, [rcx+15]
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::And,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(-16),
                },
                &[],
            )); // and rcx, ~15
                // 从 overflow_arg_area load 真 80-bit 到 st(0) (D9)。
            self.emit_ld80_load_to_st0(false); // fld tbyte (%rcx) → st(0)，无损
                                               // rcx += 16
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::Add,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        1,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Imm(16),
                },
                &[],
            )); // add rcx, 16
                // 写回推进后的 overflow_arg_area 到 [r11+8] (r11 = va_list 地址)。
                // 旧编码 0x4C,0x89,0x4B 实际是 mov [rbx+8],r9 (REX.R=1 无 REX.B,
                // reg→r9, rm→rbx), 写入错误基址导致 stdarg-1 segfault; 正确的
                // mov [r11+8],rcx 需 REX.B (reg=rcx 源, rm=r11 目标)。
            self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B64,
                1,
                11,
                8,
            ));
            return;
        }
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = gp_offset (rcx + 0)
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                rip_relative: false,
                width: crate::codegen::shared::asm_operand::Width::B32,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B32,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Mem(
                    crate::codegen::x86_64::opcode::MemOperand {
                        disp: 0,
                        base: Some(1),
                        index: None,
                        seg: None,
                    },
                ),
            },
            &[],
        )); // mov eax, (rcx)
        self.emit_insn(&Insn::alu_rm_reg_imm8(7, 0, 0x30)); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + gp_offset: 从 (rcx+16) 加载到 r11
        // REX 0x4C: R=1 使 reg=r11, B=0 使 rm=rcx (rm=001+REX.B 会变 r9)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            16,
        )); // mov r11, 16(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        let sz = arg_ty.size();
        let signed = Self::va_arg_ty_signed(arg_ty);
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x41, &[0x04, 0x03]),
            2 => self.emit_va_arg_int_load(2, signed, 0x41, &[0x04, 0x03]),
            4 => self.emit_va_arg_int_load(4, signed, 0x41, &[0x04, 0x03]),
            _ => self.emit_va_arg_int_load(8, signed, 0x41, &[0x04, 0x03]),
        }
        // gp_offset += step
        self.emit_insn(&Insn::alu_mem_imm8_b32(0, 1, 0, step as i32)); // add dword [rcx], imm8
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        // overflow_arg_area: 从 (rcx+8) 加载
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            8,
        )); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x41, &[0x03]),
            2 => self.emit_va_arg_int_load(2, signed, 0x41, &[0x03]),
            4 => self.emit_va_arg_int_load(4, signed, 0x41, &[0x03]),
            _ => self.emit_va_arg_int_load(8, signed, 0x41, &[0x03]),
        }
        self.push(0);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            8,
        )); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 11, step as i64)); // add r11, step
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            8,
        )); // mov 8(rcx), r11 (REX.R=1: reg=r11)
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    fn gen_va_arg_ptr_fp(&mut self, arg_ty: &Type, dst: u8) {
        let reg_step = 16i32;
        let overflow_step = 8i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = fp_offset (rcx + 4)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B32,
            0,
            1,
            4,
        )); // mov eax, 4(rcx)
        self.emit_insn(&Insn::cmp_eax_imm32(176)); // cmp $176, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + fp_offset
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            16,
        )); // mov r11, 16(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        if matches!(arg_ty, Type::Float) {
            // movss (r11,rax), xmm0: F3 41 0F 10 04 03
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                    width: crate::codegen::x86_64::opcode::SimdWidth::SS,
                    opcode10: false,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B128,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(11),
                            index: Some((0, 1)),
                            seg: None,
                        },
                    ),
                },
                &[],
            ));
        } else {
            // movsd (r11,rax), xmm0: F2 41 0F 10 04 03
            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                    width: crate::codegen::x86_64::opcode::SimdWidth::SD,
                    opcode10: false,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B128,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(11),
                            index: Some((0, 1)),
                            seg: None,
                        },
                    ),
                },
                &[],
            ));
        }
        // fp_offset += reg_step
        self.emit_insn(&Insn::alu_mem_imm8_b32(0, 1, 4, reg_step)); // add dword [rcx+4], reg_step
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        // overflow_arg_area
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            8,
        )); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        if matches!(arg_ty, Type::Float) {
            self.emit_insn(&Insn::simd_mov_mem_base(
                false,
                crate::codegen::x86_64::opcode::SimdWidth::SS,
                0,
                11,
                0,
            )); // movss (r11), xmm0
        } else {
            self.emit_insn(&Insn::simd_mov_mem_base(
                false,
                crate::codegen::x86_64::opcode::SimdWidth::SD,
                0,
                11,
                0,
            )); // movsd (r11), xmm0
        }
        self.push(0);
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            8,
        )); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        self.emit_insn(&Insn::alu_rm_reg_imm8_b64(0, 11, overflow_step as i64)); // add r11, overflow_step
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B64,
            11,
            1,
            8,
        )); // mov 8(rcx), r11 (REX.R=1: reg=r11)
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    pub(crate) fn gen_va_arg_ptr_struct(&mut self, arg_ty: &Type) {
        let sz = arg_ty.size();
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32;
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });

        // 从 va_list 结构体 [rcx] 加载字段:
        //   rax = gp_offset, rdx = fp_offset, r8 = overflow_arg_area
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                rip_relative: false,
                width: crate::codegen::shared::asm_operand::Width::B32,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B32,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Mem(
                    crate::codegen::x86_64::opcode::MemOperand {
                        disp: 0,
                        base: Some(1),
                        index: None,
                        seg: None,
                    },
                ),
            },
            &[],
        )); // mov eax, [rcx]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B32,
            2,
            1,
            4,
        )); // mov edx, [rcx+4]
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            8,
            1,
            8,
        )); // mov r8, [rcx+8]

        // 先把 va_list 指针留在 r11 (溢出区的 `rep movsb` 会毁 rcx, 且下面的
        // 偏移回写要靠 r11), 再把 reg_save_area 装进 rcx —— 装载体
        // `emit_va_arg_reg_save_load` 与 named / global 两个变体共用同一份,
        // 而那份固定以 `(%rcx,%rax)` / `(%rcx,%rdx)` 解引用。此前这里把
        // reg_save_area 装进 r9 却让装载体用 rcx 当基址 (编码 `4C 8B 0C 01`
        // 缺 REX.B)，于是读的是 **va_list 结构体自身 + gp_offset** 的栈垃圾
        // (D1：指针型 va_list 参数读聚合全错)。
        self.push(3); // push r11
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 11, 1)); // mov r11, rcx (保存 va_list 指针)
        self.emit_insn(&Insn::mov_rm_mem(
            true,
            crate::codegen::shared::asm_operand::Width::B64,
            1,
            1,
            16,
        )); // mov rcx, [rcx+16] = reg_save_area

        // 与 named / global 两个落点共用同一决策体。旧副本在此额外加了
        // `align >= 16` 的“高对齐 struct 强制走溢出区”近似：GCC 的判据只看
        // 分类，于是 `__m128d`（align 16、分类 [Sse, SseUp]）在本可从保存区
        // 取时被强行推到 overflow_arg_area，而 caller 把它留在 xmm 里。
        self.emit_va_arg_agg_body(&classes, sz, align, step, None, has_memory, (11, 0));

        // 恢复 va_list 指针
        self.pop(3); // pop r11
    }
    /// 32 位寄存器写入 `[base+disp]`（`va_list` 的 `gp_offset`/`fp_offset` 回写）。
    /// 与 `emit_store_reg_mem` 同构，只是不带 REX.W：三个字段的宽度不同（两个
    /// `u32` + 一个指针），按 `emit_store_reg_mem` 直接存 64 位会越界写坏相邻
    /// 字段。三个落点的偏移回写收敛到这里，基址/偏移由 `wb` 描述。
    fn emit_store_reg32_mem(&mut self, base: u8, disp: i32, src: u8) {
        // mov [base+disp], r32 (0x89, no REX.W; REX.R/B 仅在 src/base>=8 时由编码器自动发).
        self.emit_insn(&Insn::mov_rm_mem(
            false,
            crate::codegen::shared::asm_operand::Width::B32,
            src,
            base,
            disp,
        ));
    }

    /// 聚合变参装载的**唯一**决策体 + 两路装载体（named 局部 / 全局 / 已退化为
    /// 指针的 `va_list` 三个落点共用）。
    ///
    /// 调用前寄存器约定（由各落点自行准备，见三个 `gen_va_arg_*struct*`）：
    ///   `rax` = gp_offset、`rdx` = fp_offset、`rcx` = reg_save_area、
    ///   `r8` = overflow_arg_area、`rdi` = 聚合目的缓冲区。
    /// `wb = (base, off)` 描述游标回写地址：`gp_offset = [base+off]`、
    /// `fp_offset = [base+off+4]`、`overflow_arg_area = [base+off+8]`。
    ///
    /// 四条轴在此收敛为同一真源：判定用 `va_arg_reg_counts`（**XMM 寄存器个数**
    /// 而非 eightbyte 个数）、装载槽布局用 `va_arg_sse_disp`（`Sse` 开新 16 字节
    /// 槽、`SseUp` 续用前槽高半）、溢出区对齐用调用点传入的 `align`、游标推进用
    /// `step = (size+7)&!7`。历史上这三个落点各持一份逐字副本，单点修复会静默
    /// 失效，且副本间已实际漂移：
    /// - named 副本在溢出路径末尾**漏了 `jmp done`**（还把 `done` 标号提前
    ///   注册在这里），于是溢出装载完直接落进寄存器装载体，用被 `rep movsb`
    ///   清零的 `rcx` 继续解引用 → SIGSEGV；
    /// - named/global 两份把 `mov r11, step`（立即数）写成 `49 89 C3`
    ///   = `mov r11, rax`，又把 `add r8, r11` 写成 `49 01 D0` = `add r8, rdx`
    ///   （ModRM reg 字段错），于是溢出游标按 gp/fp_offset 乱走。
    fn emit_va_arg_agg_body(
        &mut self,
        classes: &[AbiClass],
        sz: usize,
        align: i32,
        step: i32,
        vla_size_reg: Option<u8>,
        force_mem: bool,
        wb: (u8, i32),
    ) {
        let (n_gp, n_xmm) = Self::va_arg_reg_counts(classes);
        let (wb_base, wb_off) = wb;
        let overflow = self.fresh_label();
        let regs = self.fresh_label();
        let done = self.fresh_label();
        if force_mem {
            self.jmp_rel32(&overflow, JumpKind::Jmp);
        } else {
            // 本对象占用的**每一个**槽都必须还在保存区内：
            // `offset > MAX - n*step` → 溢出（GCC 真值 `cmpl $175/159; ja`，与
            // caller 侧 `fits_fp = fp + need_fp <= 8` 是同一判据的两侧）。
            self.emit_insn(&Insn::cmp_eax_imm32(48 - 8 * n_gp)); // cmp eax, imm32
            self.jmp_rel32(&overflow, JumpKind::Jg);
            if n_xmm > 0 {
                // cmp edx, imm32: B32 0x81 /7 id (no REX.W)
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Cmp,
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            2,
                            crate::codegen::shared::asm_operand::Width::B32,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(
                            (176 - 16 * n_xmm) as i64,
                        ),
                    },
                    &[],
                ));
                self.jmp_rel32(&overflow, JumpKind::Jg);
            }
            self.jmp_rel32(&regs, JumpKind::Jmp);
        }

        // ---- 溢出区路径: 对齐 + rep movsb + 推进 ----
        // SysV ABI: caller (emit_sysv_call) force_mem 的参数按其声明对齐放置.
        // callee 必须在 overflow_arg_area 上做相同对齐, 否则中间有 padding
        // 时 (如 align=16 的 agg 前面有 force_mem int) 会错位.
        // GCC 实测：align<=8 的 `struct sd` 不做任何 rounding，align=16 的
        // `__m128d` 才 `addq $15; andq $-16` —— 故只在 align>8 时插入对齐。
        self.label(&overflow);
        if align > 8 {
            // r8 = (r8 + align - 1) & ~(align - 1)  — 向上对齐到 agg align
            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 8, (align - 1) as i64)); // add r8, imm32
            let mask: i32 = !(align - 1);
            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 8, mask as i64)); // and r8, imm32
        }
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 6, 8)); // mov rsi, r8
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
        match vla_size_reg {
            // VLA: 运行时尺寸在 `sr`（不能放 rcx/rax：rcx = reg_save_area 已在
            // 字段装载时占用，而 `mov rcx, sz` 本身是 `rep movsb` 的计数）。
            Some(sr) => {
                // lea rax, [sr+7]: REX.W R/B 取决于 sr>=8. disp=7 装 i8, mod=01.
                self.emit_insn(&Insn::lea_reg_base_disp(0, sr, 7)); // lea rax, [sr+7]
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::And,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(-8),
                    },
                    &[],
                )); // and rax, -8
                self.emit_insn(&Insn::mov_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B64,
                    11,
                    0,
                )); // mov r11, rax (暂存 step)
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
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
                )); // mov rcx, rax (计数 = 对齐后尺寸)
            },
            None => {
                self.emit_insn(&Insn::mov_reg_imm32_b64(1, sz as i32)); // mov rcx, imm32 (= sz)
            },
        }
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // rep movsb
                                                                                         // 推进 overflow_arg_area: 当前对齐位置 + step。step 要么是编译期常量
                                                                                         // (imm32)，要么在 r11 (VLA)；两者编码必须分开写——旧副本用同一条
                                                                                         // `49 01 D0`（= `add r8, rdx`）冒充两者，两路都错。
        match vla_size_reg {
            Some(_) => {
                // add r8, r11: 4C 01 D8 (REX.WRB: W|R=1(r11=3? no, r11=11>=8), B=1(r8>=8))
                self.emit_insn(&Insn::alu_rm_reg_reg(
                    crate::codegen::x86_64::opcode::AluOp::Add,
                    crate::codegen::shared::asm_operand::Width::B64,
                    8,
                    11,
                ));
            },
            None => {
                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 8, step as i64)); // add r8, imm32
            },
        }
        // mov [wb_base + wb_off + 8], r8
        self.emit_store_reg_mem(wb_base, wb_off + 8, 8);
        // 溢出装载已完成，必须跳出：旧 named 副本缺这条 `jmp done`（且把
        // `done` 标号注册在此处），于是 fall through 进寄存器装载体。
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 寄存器保存区路径：按分类逐 eightbyte 搬运 ----
        self.label(&regs);
        self.emit_va_arg_reg_save_load(classes);
        // 回写推进后的游标 (rax/rdx 已是最终值)
        self.emit_store_reg32_mem(wb_base, wb_off, 0); // [wb] = eax (gp_offset)
        self.emit_store_reg32_mem(wb_base, wb_off + 4, 2); // [wb+4] = edx (fp_offset)
        self.label(&done);
    }

    /// named 变体公共前置：把 `va_list` 四字段装进 rax/rdx/rcx/r8。
    fn emit_va_list_fields_slot(&mut self, ap_off: i32) {
        use crate::codegen::shared::asm_operand::Width;
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B32, 0, 13, ap_off)); // mov eax, [r13+disp32] (gp_offset)
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B32, 2, 13, ap_off + 4)); // mov edx, [r13+disp32] (fp_offset)
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 1, 13, ap_off + 16)); // mov rcx, [r13+disp32] (reg_save_area)
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 8, 13, ap_off + 8)); // mov r8, [r13+disp32] (overflow_arg_area)
    }
    pub(crate) fn gen_va_arg_struct(&mut self, ap_name: &str, arg_ty: &Type) {
        // 全局 va_list: 直接 rip 相对访问 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_struct_global(ap_name, arg_ty);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        let has_vla = Self::type_has_vla(arg_ty);
        let sz = arg_ty.size();
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32; // 计算编译期步长，非VLA使用

        // 处理 VLA: 使用运行时尺寸（存 r9，避开下面被 reg_save_area 占用的 rcx）
        if has_vla {
            self.gen_sizeof_runtime(arg_ty, 9);
        }

        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });
        // GCC 实测：判据只看分类（`long double`/聚合走溢出），**不看对齐**。旧
        // 指针变体额外加了 `align >= 16` 的近似，于是 16 字节对齐的
        // `__m128d`/`_Alignas(16)` 聚合在本可走保存区时被强行推到溢出区。
        self.emit_va_list_fields_slot(ap_off as i32);
        self.emit_va_arg_agg_body(
            &classes,
            sz,
            align,
            step,
            if has_vla { Some(9) } else { None },
            has_memory,
            (13, ap_off as i32),
        );
    }
    fn gen_va_arg_struct_global(&mut self, ap_name: &str, arg_ty: &Type) {
        let has_vla = Self::type_has_vla(arg_ty);
        let sz = arg_ty.size();
        if has_vla {
            self.gen_sizeof_runtime(arg_ty, 9); // 运行时尺寸存 r9 (见 named 变体说明)
        }
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32;
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });

        // 加载 va_list 字段: rax=gp_offset, rdx=fp_offset,
        // rcx=reg_save_area, r8=overflow_arg_area, r10=&va_list
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        use crate::codegen::shared::asm_operand::Width;
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B32, 0, 10, 0)); // mov eax, [r10]
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B32, 2, 10, 4)); // mov edx, [r10+4]
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 1, 10, 16)); // mov rcx, [r10+16]
        self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 8, 10, 8)); // mov r8, [r10+8]

        self.emit_va_arg_agg_body(
            &classes,
            sz,
            align,
            step,
            if has_vla { Some(9) } else { None },
            has_memory,
            (10, 0),
        );
    }
    pub(crate) fn resolve_va_list_addr(&mut self, ap: &Expr) -> VaListAddress {
        let ap_is_ptr = matches!(ap.ty(), Type::Ptr(_));
        if let Expr::Ident { name, .. } = ap {
            if ap_is_ptr {
                // va_list 作为函数参数时退化为指针，加载指针值到 rcx
                self.gen_expr(ap, Self::EVAL_BASE);
                self.mov(1, 0); // rcx = rax
                VaListAddress::InReg
            } else {
                VaListAddress::Named(name.name.clone())
            }
        } else if let Expr::Deref { ref e, .. } = ap {
            // *ptr: 加载指针值到 rcx
            self.gen_expr(e, Self::EVAL_BASE);
            self.mov(1, 0); // rcx = rax
            VaListAddress::InReg
        } else if ap_is_ptr {
            // 指针类型表达式 (如数组元素): 加载指针值到 rcx
            self.gen_expr(ap, Self::EVAL_BASE);
            self.mov(1, 0);
            VaListAddress::InReg
        } else if self.gen_lvalue_addr(ap, 1) {
            // lvalue (成员/下标): gen_lvalue_addr 已将地址放入 rcx
            VaListAddress::InReg
        } else {
            // 无法解析: 返回零地址
            self.mov_imm(1, 0);
            VaListAddress::InReg
        }
    }
    fn va_arg_ty_signed(ty: &Type) -> bool {
        !matches!(
            ty,
            Type::Bool
                | Type::UChar
                | Type::UShort
                | Type::UInt
                | Type::ULong
                | Type::ULongLong
                | Type::UInt128
        )
    }
    /// `va_list` 对象在本帧里的真实尺寸（24B = 3 槽）。规划期的晚分配预算读的就是它，
    /// 所以不得在别处再写一份「3 槽」。
    pub(crate) fn va_list_slot_ty() -> Type {
        Type::Array(Box::new(Type::ULong.into()), ArraySize::Const(3))
    }
}
