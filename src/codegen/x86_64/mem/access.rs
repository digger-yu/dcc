//! mem/access.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
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

/// 消费点读到了后端**还没接线**的家（`Spill`/`Imm`/非 GP 通道）。
/// 与 `report_internal_bug` 同族：它不是用户代码错误，而是「新增了一种家却没改消费点」。
const DIAG_UNPLUMBED_HOME: &str = "E1129";
impl Codegen {
    pub(crate) fn copy_r13_bytes(&mut self, src_off: i64, dst_off: i64, size: usize) {
        self.emit_lea_r13(6, src_off);
        self.emit_lea_r13(7, dst_off);
        self.mov_imm(1, size as i128);
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb));
    }
    fn emit_address_descriptor(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        original_expr: &Expr,
        dst: u8,
    ) -> bool {
        use crate::semantics::{AddressBase, ByteOffset};

        // 先检查是否能完整处理: 运行时偏移无法物化, 必须整体退回
        if matches!(
            &desc.offset,
            ByteOffset::Runtime { .. } | ByteOffset::VlaRuntime { .. } | ByteOffset::Unsupported
        ) {
            return false;
        }
        // Upvar 暂未实现
        if matches!(&desc.base, AddressBase::UpvarLoad { .. }) {
            return false;
        }

        // Step 1: 发射基址计算指令
        match &desc.base {
            AddressBase::StackFrame { offset } => {
                self.emit_lea_r13(dst, *offset);
            },
            AddressBase::StackSlotLoad { offset } => {
                self.load_stack(dst, *offset);
            },
            AddressBase::GlobalSymbolAddr { symbol_key } => {
                self.emit_global_addr(dst, symbol_key);
            },
            AddressBase::GlobalSymbolLoad { symbol_key } => {
                self.emit_global_load(dst, symbol_key);
            },
            AddressBase::UpvarLoad { .. } => {
                return false;
            },
            AddressBase::ComputedAddr { expr } => {
                self.gen_expr(expr, dst);
            },
            AddressBase::Unknown => {
                return false;
            },
            AddressBase::ExprAddr { sub } => {
                // `*p` 形态: 递归物化子描述符得到的就是 `p` 的**值** (目标地址)。
                // 这里绝不能 `return` 递归结果 —— 本层 `desc.offset` (由内层常量
                // 下标 / 字段偏移累加而来) 会被整个丢掉: `(*(e + 2))[1]` 的 `+8`
                // 即此, 表现为内层常量下标被静默抹掉, 恒读到 `[0]`。
                // 与下方 `PointerValue` 臂同构: 拿到基址后一律落到 Step 2。
                if !self.emit_address_descriptor(sub, original_expr, dst) {
                    return false;
                }
            },
            AddressBase::PointerValue { sub } => {
                if !self.emit_address_descriptor(sub, original_expr, dst) {
                    return false;
                }
                if dst == 0 {
                    self.emit_load_mem(8);
                } else {
                    self.push(dst);
                    self.pop(0);
                    self.emit_load_mem(8);
                    self.mov(dst, 0);
                }
            },
        }

        // Step 2: 发射常量偏移
        if let ByteOffset::Constant(off) = &desc.offset {
            if *off != 0 {
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                if dst != 0 {
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: 该寄存器只在 r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    let gas = format!("add ${}, {}", off, GAS_REG64[(dst & 15) as usize]);
                    let mut bytes = vec![rex, 0x81, 0xC0 | (dst & 7)];
                    bytes.extend_from_slice(&(*off as i32).to_le_bytes());
                    self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                } else {
                    let gas = format!("add ${}, %rax", off);
                    let mut bytes = vec![0x48, 0x81, 0xC0];
                    bytes.extend_from_slice(&(*off as i32).to_le_bytes());
                    self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                }
            }
        }

        true
    }
    fn emit_address_descriptor_with_index(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        original_expr: &Expr,
        dst: u8,
    ) -> bool {
        use crate::semantics::{AddressBase, ByteOffset};

        // 先尝试常量偏移快速路径
        if matches!(&desc.offset, ByteOffset::Constant(_)) {
            return self.emit_address_descriptor(desc, original_expr, dst);
        }
        // 语义层无法表达的偏移: 整体退回调用方旧路径
        if matches!(&desc.offset, ByteOffset::Unsupported) {
            return false;
        }
        // Upvar 暂未实现
        if matches!(&desc.base, AddressBase::UpvarLoad { .. }) {
            return false;
        }

        // 基址恒算到 rax (寄存器 0), 索引进 rcx (寄存器 1)。
        // 不能把基址直接算到 dst: 当 dst==1 时它与索引寄存器 rcx 重合,
        // 保存/恢复基址的 push/pop 会覆盖刚求出的索引, 使地址被错算成
        // base*stride (见 loop_opt sum_array 的 arr[i] 段错误)。
        const BASE: u8 = 0; // rax
        const INDEX: u8 = 1; // rcx

        // Step 1: 发射基址计算到 rax
        match &desc.base {
            AddressBase::StackFrame { offset } => {
                self.emit_lea_r13(BASE, *offset);
            },
            AddressBase::StackSlotLoad { offset } => {
                self.load_stack(BASE, *offset);
            },
            AddressBase::GlobalSymbolAddr { symbol_key } => {
                self.emit_global_addr(BASE, symbol_key);
            },
            AddressBase::GlobalSymbolLoad { symbol_key } => {
                self.emit_global_load(BASE, symbol_key);
            },
            AddressBase::UpvarLoad { .. } => {
                return false;
            },
            AddressBase::ComputedAddr { expr } => {
                self.gen_expr(expr, BASE);
            },
            AddressBase::Unknown => {
                return false;
            },
            AddressBase::ExprAddr { sub } => {
                // 递归: 子描述符也可能有运行时偏移, 但我们只处理一层
                // (两层运行时偏移需要额外临时寄存器, 暂退回 false)
                if matches!(&sub.offset, ByteOffset::Runtime { .. } | ByteOffset::VlaRuntime { .. })
                {
                    return false;
                }
                if !self.emit_address_descriptor(sub, original_expr, BASE) {
                    return false;
                }
            },
            AddressBase::PointerValue { sub } => {
                if matches!(&sub.offset, ByteOffset::Runtime { .. } | ByteOffset::VlaRuntime { .. })
                {
                    return false;
                }
                if !self.emit_address_descriptor(sub, original_expr, BASE) {
                    return false;
                }
                self.emit_load_mem(8);
            },
        }

        // Step 2: 处理运行时偏移, 偏移结果放在 rcx
        match &desc.offset {
            ByteOffset::Constant(_) => {
                // 已在上面快速路径处理
            },
            ByteOffset::Unsupported => {
                return false;
            },
            ByteOffset::Runtime { terms, addend } => {
                // 逐项累加 (`a[i][j][k]` 三层下标 = 3 项) —— 每一项自带下标
                // 表达式, 不需要调用点猜「本层 idx 是不是那个运行时下标」。
                if terms.is_empty() {
                    return false;
                }
                // 保存基址, 求值索引到 rcx, 恢复基址到 rax, 计算偏移到 rcx
                self.push(BASE);
                for (k, term) in terms.iter().enumerate() {
                    self.gen_expr(&term.index, INDEX); // rcx = 本层下标
                    if term.stride != 1 {
                        let stride_i32 = term.stride as i32;
                        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x69, 0xC9])); // imul rcx, rcx, imm32
                        self.emit_insn(&Insn::raw_bytes(&stride_i32.to_le_bytes()));
                    }
                    if k > 0 {
                        // add rcx, [rsp]; add rsp, 8  (合并上一层的部分和)
                        self.emit_insn(&Insn::alu_rm_mem(
                            crate::codegen::x86_64::opcode::AluOp::Add,
                            crate::codegen::shared::asm_operand::Width::B64,
                            1,
                            4,
                            0,
                        ));
                        self.emit_insn(&Insn::add_rsp_imm(8));
                    }
                    if k + 1 < terms.len() {
                        self.push(INDEX); // 部分和入栈, 供下一层合并
                    }
                }
                self.pop(BASE); // rax = base (不能 pop 回 rcx, 否则覆盖索引)
                                // add rcx, addend
                if *addend != 0 {
                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 1, *addend)); // add rcx, imm32
                }
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x01, 0xC8], "add %rcx, %rax")); // add rax, rcx
            },
            ByteOffset::VlaRuntime { index, addend } => {
                // VLA: stride 在运行时确定, 需要从 VLA 元数据槽加载
                // 先找 VLA stride slot
                let stride_slot = self.find_vla_stride_slot_from_desc(desc);
                if stride_slot.is_none() {
                    return false;
                }
                let slot = stride_slot.unwrap();
                self.push(BASE);
                self.gen_expr(index, INDEX); // rcx = index
                                             // mov rdx, [r13+slot]; imul rcx, rdx
                self.emit_mov_mem_r13(2, slot);
                self.emit_insn(&Insn::imul_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B64,
                    1, 2, // rcx, rdx
                ));
                self.pop(BASE); // rax = base
                if *addend != 0 {
                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 1, *addend)); // add rcx, imm32
                }
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x01, 0xC8], "add %rcx, %rax")); // add rax, rcx
            },
        }

        // 最终地址在 rax, 按需搬到调用方要求的 dst
        if dst != BASE {
            self.mov(dst, BASE);
        }

        true
    }
    pub(crate) fn emit_func_addr(&mut self, dst: u8, name: &str) {
        let rex_base = 0x48u8 | if dst >= 8 { 0x44 } else { 0 };
        let modrm = 0x05 | ((dst & 7) << 3);
        if !self.relocatable {
            // ET_EXEC: LEA + label_addr_refs
            self.emit_insn(&Insn::raw_bytes(&[rex_base, 0x8D, modrm, 0, 0, 0, 0]));
            let disp32_pos = self.pos() - 4;
            self.label_addr_refs.push((disp32_pos, name.to_string()));
        } else if self.defined_funcs.contains(name)
            || self.nested_funcs.contains_key(name)
            || self.target.kernel_code_model
        {
            // 内部函数或内核代码模型: LEA dst, [RIP+disp32] → 直接地址
            // 内核不使用 GOT, 所有符号在链接时直接解析
            self.emit_insn(&Insn::raw_bytes(&[rex_base, 0x8D, modrm, 0, 0, 0, 0]));
            let disp32_pos = self.pos() - 4;
            self.pending_func_rels.push((disp32_pos, name.to_string()));
        } else {
            // 外部/未定义函数 (small model): MOV dst, [RIP+disp32] → GOT 间接
            self.emit_insn(&Insn::raw_bytes(&[rex_base, 0x8B, modrm, 0, 0, 0, 0]));
            let disp32_pos = self.pos() - 4;
            // MOV 指令加载 GOT 条目地址 → GOTPCREL
            self.pending_func_addr_rels
                .push((disp32_pos, name.to_string(), false));
        }
    }
    /// 登记全局符号 RIP-relative 引用的重定位。
    /// `disp_pos` = 指令内 disp32 首字节绝对位置（fld/fstp/mov 各形态通用）。
    pub(crate) fn push_global_float_rel(&mut self, disp_pos: u64, name: &str) {
        let resolved = self.resolve_global_key(name).to_string();
        // TLS 全局变量: ET_EXEC 走 pending_tls_rels 由 resolve() 修补,
        // ET_REL 走 pending_data_rels + R_X86_64_PC32 (与整数路径一致)
        let is_tls = self
            .globals_raw
            .get(&resolved)
            .map(|g| g.is_tls)
            .unwrap_or(false);
        if is_tls && !self.relocatable {
            self.pending_tls_rels.push((disp_pos, resolved));
        } else {
            self.pending_data_rels.push((disp_pos, resolved));
        }
    }

    /// 全局标量浮点 rvalue load 的**单一真源**：
    /// - st0 通道（`long double`，或 `-mno-sse`）发 RIP-relative `fld`
    ///   （float=`D9 /5`、double=`DD /5`、long double=`DB /5 m80`，6 字节）；
    /// - xmm 通道保留 movss/movsd（8 字节）。
    /// 旧写法对 `long double` 发 movsd，只读走真 80-bit 槽的低 8 字节，
    /// 全局量读回恒为 `-nan`（局部路径早已修为 fldt，这是唯一漏网标量站点）。
    pub(crate) fn emit_global_float_load(&mut self, name: &str, ty: &Type) {
        // _Imaginary: 按内型加载
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            &inner.base
        } else {
            ty
        };
        if self.float_in_st0(effective_ty) {
            let (opcode, modrm) = match effective_ty {
                Type::Float => (0xD9u8, 0x05u8),
                Type::LongDouble => (0xDBu8, 0x2Du8),
                _ => (0xDDu8, 0x05u8),
            };
            // fld m32/m64/m80 [rip+disp32]：disp32 位于字节 2（指令长 6）。
            // 段限定全局走同一个口：前缀在 opcode 之前，pos 在其后取。
            let resolved = self.resolve_global_key(name).to_string();
            self.emit_global_seg(&resolved);
            let disp_pos = self.pos() + 2;
            self.emit_insn(&Insn::raw_bytes(&[opcode, modrm, 0, 0, 0, 0]));
            self.push_global_float_rel(disp_pos, name);
            return;
        }
        self.emit_global_float_load_to_xmm(0, name, ty);
    }

    pub(crate) fn emit_global_float_load_to_xmm(&mut self, xmm: u8, name: &str, ty: &Type) {
        // _Imaginary: 按内型加载
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            &inner.base
        } else {
            ty
        };
        let is_float = matches!(effective_ty, Type::Float);
        let mut rex = 0u8;
        if xmm >= 8 {
            rex |= 0x04;
        }
        let modrm = 0x05 | ((xmm & 7) << 3);
        let mut ins = Vec::new();
        // mandatory prefix 必须在 REX 之前
        if is_float {
            ins.push(0xF3);
        } else {
            ins.push(0xF2);
        }
        if rex != 0 {
            ins.push(0x40 | rex);
        }
        ins.extend_from_slice(&[0x0F, 0x10, modrm, 0, 0, 0, 0]);
        let resolved = self.resolve_global_key(name).to_string();
        self.emit_global_seg(&resolved);
        let disp_pos = self.pos() + ins.len() as u64 - 4;
        self.emit_insn(&Insn::raw_bytes(&ins));
        self.push_global_float_rel(disp_pos, name);
    }
    pub(crate) fn emit_global_float_load_to_xmm_offset(
        &mut self,
        xmm: u8,
        name: &str,
        offset: i32,
        ty: &Type,
    ) {
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            &inner.base
        } else {
            ty
        };
        let is_float = matches!(effective_ty, Type::Float);
        // 加载全局地址到 rax
        self.emit_global_addr(0, name);
        // 加偏移
        if offset != 0 {
            if (-128..=127).contains(&offset) {
                let gas = format!("add ${}, %rax", offset);
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC0, offset as u8], &gas));
            } else {
                let gas = format!("add ${}, %rax", offset);
                let mut bytes = vec![0x48, 0x05];
                bytes.extend_from_slice(&offset.to_le_bytes());
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            }
        }
        // 从 [rax] 加载到 xmm
        let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
        let op_name = if is_float { "movss" } else { "movsd" };
        let mut xmm_bytes: Vec<u8> = Vec::new();
        xmm_bytes.push(pfx);
        if xmm >= 8 {
            xmm_bytes.push(0x44);
        }
        xmm_bytes.extend_from_slice(&[0x0F, 0x10, 0x00]);
        let gas = format!("{}, (%rax), %xmm{}", op_name, xmm);
        self.emit_insn(&Insn::raw_with_gas(&xmm_bytes, &gas));
    }

    /// 全局量 `name + offset` 的 x87 `fld`（用于 `_Complex long double`
    /// 全局量的实/虚部分量加载）。前置：ra 已无需由本函数准备；
    /// 本函数自取全局地址加偏移后发 `fld [addr]`，结果交付 st(0)。
    /// float=`D9 /0`、double=`DD /0`、long double=`DB /5 m80`。
    pub(crate) fn emit_global_float_load_x87_offset(&mut self, name: &str, offset: i32, ty: &Type) {
        self.emit_global_addr(0, name);
        if offset != 0 {
            if (-128..=127).contains(&offset) {
                let gas = format!("add ${}, %rax", offset);
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x83, 0xC0, offset as u8], &gas));
            } else {
                let gas = format!("add ${}, %rax", offset);
                let mut bytes = vec![0x48, 0x05];
                bytes.extend_from_slice(&offset.to_le_bytes());
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            }
        }
        let (opcode, reg_field) = match ty {
            Type::Float => (0xD9u8, 0u8),
            Type::LongDouble => (0xDBu8, 5u8),
            _ => (0xDDu8, 0u8),
        };
        self.emit_insn(&Insn::raw_bytes(&[opcode, (reg_field << 3) | 0x00]));
    }
    /// 全局标量浮点 store 的**单一真源**。
    /// 前置：`src_ty` 的 rvalue 已按通道约定求值（整数在 rax、float/double 在
    /// xmm0、`long double` 在 st(0)）。
    /// - st0 通道（`long double` / `-mno-sse`）：先把源操作数拓宽到 st(0)，
    ///   再发 RIP-relative `fstp`（float=`D9 /3`、double=`DD /3`、
    ///   long double=`DB /7 m80`），落真 80-bit 槽并弹出，与局部
    ///   `emit_store_scalar_float_at` 语义一致；
    /// - xmm 通道保留 movss/movsd。
    /// 旧写法对 `long double` 发 movsd + 手工清零高 8 字节，把陈旧 xmm0 的
    /// f64 位型写进真 80-bit 对象（且 st(0) 漏在 x87 栈上），赋值后读回 -nan。
    pub(crate) fn emit_global_float_store(&mut self, name: &str, dst_ty: &Type, src_ty: &Type) {
        let effective_ty = if let Type::Imaginary(ref inner) = dst_ty {
            &inner.base
        } else {
            dst_ty
        };
        if self.float_in_st0(effective_ty) {
            // 源操作数 → st(0)（整数/f32/f64 经精确拓宽，long double 已在 st0）。
            self.emit_operand_to_st0(src_ty, 0);
            let (opcode, modrm) = match effective_ty {
                Type::Float => (0xD9u8, 0x1Du8),
                Type::LongDouble => (0xDBu8, 0x3Du8),
                _ => (0xDDu8, 0x1Du8),
            };
            let resolved = self.resolve_global_key(name).to_string();
            self.emit_global_seg(&resolved);
            let disp_pos = self.pos() + 2;
            self.emit_insn(&Insn::raw_bytes(&[opcode, modrm, 0, 0, 0, 0]));
            self.push_global_float_rel(disp_pos, name);
            return;
        }
        let resolved = self.resolve_global_key(name).to_string();
        let (_, is_tls, _off) = self
            .globals_raw
            .get(&resolved)
            .map(|g| (g.size, g.is_tls, g.offset))
            .unwrap_or((8, false, 0));
        let tls_exec = is_tls && !self.relocatable;
        let is_float = matches!(effective_ty, Type::Float);
        let pfx = if is_float { 0xF3 } else { 0xF2 };
        self.emit_global_seg(&resolved);
        let pos = self.pos() + 4;
        self.emit_insn(&Insn::raw_bytes(&[pfx, 0x0F, 0x11, 0x05, 0, 0, 0, 0]));
        if tls_exec {
            self.pending_tls_rels.push((pos, resolved.clone()));
        } else {
            self.pending_data_rels.push((pos, resolved.clone()));
        }
    }
    pub(crate) fn emit_global_float_store_from_xmm(&mut self, xmm: u8, name: &str, ty: &Type) {
        let resolved = self.resolve_global_key(name).to_string();
        let is_float = matches!(ty, Type::Float);
        let mut rex = 0u8;
        if xmm >= 8 {
            rex |= 0x04;
        }
        let modrm = 0x05 | ((xmm & 7) << 3);
        let mut ins = Vec::new();
        // mandatory prefix 必须在 REX 之前
        if is_float {
            ins.push(0xF3);
        } else {
            ins.push(0xF2);
        }
        if rex != 0 {
            ins.push(0x40 | rex);
        }
        ins.extend_from_slice(&[0x0F, 0x11, modrm, 0, 0, 0, 0]);
        self.emit_global_seg(&resolved);
        let disp_pos = self.pos() + ins.len() as u64 - 4;
        self.emit_insn(&Insn::raw_bytes(&ins));
        self.pending_data_rels.push((disp_pos, resolved));
    }

    /// 全局标量浮点 lvalue → 指定 xmm（inline asm xmm 约束输入）。
    /// long double：RIP fldt → 栈 scratch fstpt → 按 xmm 宽度
    /// fld/fstp 单步舍入 → movss/movsd；f32/f64 直接 RIP movss/movsd。
    /// 旧写法对 long double 发 RIP movsd/movq，读低 8 字节当 double。
    pub(crate) fn emit_global_float_to_xmm_typed(&mut self, xmm: u8, name: &str, ty: &Type) {
        let effective_ty = if let Type::Imaginary(ref inner) = ty {
            &inner.base
        } else {
            ty
        };
        if self.float_in_st0(effective_ty) {
            self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp,16
                                                  // fld tbyte [rip+x]
            let (opcode, modrm) = match effective_ty {
                Type::Float => (0xD9u8, 0x05u8),
                Type::LongDouble => (0xDBu8, 0x2Du8),
                _ => (0xDDu8, 0x05u8),
            };
            let resolved = self.resolve_global_key(name).to_string();
            self.emit_global_seg(&resolved);
            let dp1 = self.pos() + 2;
            self.emit_insn(&Insn::raw_bytes(&[opcode, modrm, 0, 0, 0, 0]));
            self.push_global_float_rel(dp1, name);
            self.emit_x87_fstp_at(4, 0, effective_ty); // fstp tbyte [rsp]
            let is_float = matches!(effective_ty, Type::Float);
            let pfx: u8 = if is_float { 0xF3 } else { 0xF2 };
            // REX 必须在 mandatory prefix 之后
            self.emit_insn(&Insn::raw_bytes(&[pfx]));
            if xmm >= 8 {
                self.emit_insn(&Insn::raw_bytes(&[0x44]));
            }
            self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x10, 0x04 | ((xmm & 7) << 3), 0x24]));
            self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp,16
            return;
        }
        self.emit_global_float_load_to_xmm(xmm, name, ty);
    }

    /// xmm（f32/f64）→ 全局 long double lvalue（inline asm xmm 约束
    /// 写回）：栈 scratch mov → fld 拓宽 → RIP fstpt。
    /// 旧写法发 RIP movsd，把 f64 位型写进 80-bit 对象（读回 -nan）。
    pub(crate) fn emit_xmm_to_global_float_typed(
        &mut self,
        xmm: u8,
        name: &str,
        src_ty: &Type,
        dst_ty: &Type,
    ) {
        let effective_dst = if let Type::Imaginary(ref inner) = dst_ty {
            &inner.base
        } else {
            dst_ty
        };
        let effective_src = if let Type::Imaginary(ref inner) = src_ty {
            &inner.base
        } else {
            src_ty
        };
        debug_assert!(self.float_in_st0(effective_dst));
        self.emit_insn(&Insn::sub_rsp_imm(16)); // sub rsp,16
        let pfx: u8 = if matches!(effective_src, Type::Float) {
            0xF3
        } else {
            0xF2
        };
        // REX 必须在 mandatory prefix 之后
        self.emit_insn(&Insn::raw_bytes(&[pfx]));
        if xmm >= 8 {
            self.emit_insn(&Insn::raw_bytes(&[0x44]));
        }
        self.emit_insn(&Insn::raw_bytes(&[0x0F, 0x11, 0x04 | ((xmm & 7) << 3), 0x24])); // mov [rsp],xmm
        self.emit_x87_fld_at(4, 0, effective_src); // fld [rsp]
        let (opcode, modrm) = match effective_dst {
            Type::Float => (0xD9u8, 0x1Du8),
            Type::LongDouble => (0xDBu8, 0x3Du8),
            _ => (0xDDu8, 0x1Du8),
        };
        let resolved = self.resolve_global_key(name).to_string();
        self.emit_global_seg(&resolved);
        let dp = self.pos() + 2;
        self.emit_insn(&Insn::raw_bytes(&[opcode, modrm, 0, 0, 0, 0])); // fstp [rip+x]
        self.push_global_float_rel(dp, name);
        self.emit_insn(&Insn::add_rsp_imm(16)); // add rsp,16
    }

    pub(crate) fn emit_ident_base_addr(&mut self, name: &str, arr_ty: &Type, dst: u8) {
        // 嵌套函数 upvar 数组/VLA: 对象在定义父帧中。VLA/VM struct 的
        // 父帧 [off] 是元数据槽, 槽内容才是数据基址 (二次加载);
        // 普通数组/向量的父帧+off 即数据地址 (921017-1: 嵌套函数
        // g 读外层 VLA a, 无此分支会懒分配本帧垃圾槽)。
        if self.current_func_is_nested && self.upvar_offsets.contains_key(name) {
            let parent_off = self.upvar_offsets.get(name).copied().unwrap_or(0);
            let intermediates: Vec<i64> = self
                .upvar_intermediate_offsets
                .get(name)
                .cloned()
                .unwrap_or_default();
            // 走到起源帧
            self.load_stack(dst, self.static_chain_offset);
            for &ioff in &intermediates {
                if Self::fits_disp32(ioff) {
                    let rex = 0x48u8 | if dst >= 8 { 0x05u8 } else { 0 };
                    let modrm = 0x83 | ((dst & 7) << 3);
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, modrm]));
                    self.emit_insn(&Insn::raw_bytes(&(ioff as i32).to_le_bytes()));
                } else {
                    self.mov_imm(10, ioff as i128);
                    // lea r11, [r11+r10] — P11 Bucket P typed Lea SIB.
                    let insn = Insn::new(
                        X86_64Opcode::Lea {
                            dst: Operand::Reg(11, crate::codegen::shared::asm_operand::Width::B64),
                            mem: MemOperand { disp: 0, base: Some(11), index: Some((10, 1)), seg: None },
                            rip_relative: false,
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::raw_bytes(&[0x4F, 0x8D, 0x1C, 0xD3])); // 兜底
                    }
                }
            }
            if Self::type_has_vla(arr_ty) {
                // VLA: 起源帧[off] 是元数据槽, 槽内容是数据基址 (二次加载)
                // 指令: mov dst, [dst + parent_off]  (dst 已指向起源帧)
                // modrm: mod=10(disp32), reg=dst, rm=dst
                // 原 bug 用 0x03 (mod=00, rm=rbx) 导致 mov (%rbx), dst +
                // 垃圾字节被后续指令流误解析 → SIGILL.
                if Self::fits_disp32(parent_off) {
                    let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: 自反 ModRM, reg 与 r/m 都要扩, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    let modrm = 0x80 | ((dst & 7) << 3) | (dst & 7);
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, modrm]));
                    self.emit_insn(&Insn::raw_bytes(&(parent_off as i32).to_le_bytes()));
                } else {
                    self.mov_imm(10, parent_off as i128);
                    // lea dst, [dst + r10]
                    self.emit_insn(&Insn::raw_bytes(&[0x4D, 0x8D, 0x1C, 0xD0 | (dst & 7)]));
                    // mov dst, [dst]
                    let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, (dst & 7)]));
                }
            } else {
                // 普通数组/向量: 起源帧 + parent_off 即数据地址
                if parent_off != 0 {
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                    let gas = format!("add ${}, {}", parent_off, GAS_REG64[(dst & 15) as usize]);
                    let mut bytes = vec![rex, 0x81, 0xC0 | (dst & 7)];
                    bytes.extend_from_slice(&(parent_off as i32).to_le_bytes());
                    self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                }
            }
            return;
        }
        if self.vla_offsets.contains_key(name) {
            // VLA: 基址存储在变量栈槽中
            let off = self.var_offset(name);
            self.load_stack(dst, off);
        } else if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
            // 数组/向量: 取地址 (不加载)
            let resolved_key = self.resolve_global_key(name).to_string();
            if self.globals_raw.contains_key(&resolved_key) && !self.is_local_name(name) {
                self.emit_global_addr(dst, &resolved_key);
            } else {
                let off = self.var_offset(name);
                self.emit_lea_r13(dst, off);
            }
        } else {
            // 指针/标量: 加载值 (但全局数组衰减需用 LEA)
            let resolved_key = self.resolve_global_key(name).to_string();
            let is_global_array = self
                .globals_raw
                .get(&resolved_key)
                .is_some_and(|g| matches!(g.ty, Type::Array(_, _) | Type::Vector(_, _)));
            if is_global_array && !self.is_local_name(name) {
                // 全局数组衰减: 取地址而非加载
                self.emit_global_addr(dst, &resolved_key);
            } else if self.is_global_not_local(name) {
                // 全局指针/标量: 加载值
                self.emit_global_load(dst, &resolved_key);
            } else {
                // 局部变量: 从栈加载
                let off = self.var_offset(name);
                self.load_stack(dst, off);
            }
        }
    }
    pub(crate) fn emit_index_scale(&mut self, es: i32, arr: &Expr, elem_ty: &Type) {
        // Ptr(VM struct) 索引: 元素大小是运行时值 (sizeof 语义), 不能用
        // 编译期 size() 回退值; es==1 早退前先判定, 避免回退值 1 漏乘
        if matches!(elem_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
            && elem_ty.has_vla()
        {
            if let Some(slot) = self.vm_size_slot(elem_ty) {
                self.emit_mov_mem_r13(2, slot);
                self.emit_insn(&Insn::imul_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B64,
                    1, 2, // rcx, rdx
                ));
            } else {
                self.gen_sizeof_runtime(elem_ty, 11);
                // imul rcx, r11
                self.emit_insn(&Insn::imul_rm_reg(
                    crate::codegen::shared::asm_operand::Width::B64,
                    1, 11, // rcx, r11
                ));
            }
            return;
        }
        if es == 1 {
            return;
        }
        if let Some(slot) = self.vla_runtime_stride_slot(arr, elem_ty) {
            // mov rdx, [r13+slot]; imul rcx, rdx
            self.emit_mov_mem_r13(2, slot);
            self.emit_insn(&Insn::imul_rm_reg(
                crate::codegen::shared::asm_operand::Width::B64,
                1, 2, // rcx, rdx
            ));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[0x48, 0x69, 0xC9])); // imul rcx, rcx, imm32
            self.emit_insn(&Insn::raw_bytes(&es.to_le_bytes()));
        }
    }
    pub(crate) fn emit_load_global_indexed(
        &mut self,
        dst_gp: u8,
        idx_reg: u8,
        global: &str,
        elem_size: u8,
        elem_ty: &Type,
    ) {
        // 符号本体带段限定（`__seg_gs int arr[4];` 的 `arr[i]`）：前缀只能贴在**真正做
        // 内存访问**的那一笔上（上面的 LEA 不算访问，贴了是空操作）。本函数是「全局数组
        // 下标」这一族的单一入口，所以在此处统一处理，而不是回到各个调用点分支。
        let seg = self.global_seg_prefix(global);
        let scale = match elem_size {
            2 => 1u8,
            4 => 2,
            8 => 3,
            _ => 0,
        };
        // lea r11, [rip + disp32] (RIP-relative, 链接时修补 PC 相对位移)
        let lea_pos = self.pos();
        self.emit_insn(&Insn::raw_bytes(&[0x4C, 0x8D, 0x1D, 0, 0, 0, 0]));
        self.pending_data_rels
            .push((lea_pos + 3, global.to_string()));
        let sib = (scale << 6) | ((idx_reg & 7) << 3) | 3; // base=r11 (REX.B)
        let modrm = 0x04; // mod=00, rm=100 (SIB), reg 字段在下方按目标寄存器填充
        let is_float = matches!(elem_ty, Type::Float);
        let is_double = matches!(elem_ty, Type::Double | Type::LongDouble);
        if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble)
            && self.float_in_st0(elem_ty)
        {
            // long double（及 -mno-sse 下的全部浮点）：`fld tbyte [r11 + idx*scale]`。
            // 旧写法对 LongDouble 发 movsd，只拿到低 8 字节。
            // 16 字节元素无法用 SIB scale 表达（最大 8），先把索引乘成字节偏移。
            let (opcode, rf) = match elem_ty {
                Type::Float => (0xD9u8, 0u8),
                Type::LongDouble => (0xDBu8, 5u8),
                _ => (0xDDu8, 0u8),
            };
            let (scale2, sib2) = if elem_size == 16 {
                // imul idx, idx, 16
                let rex_i = 0x48u8 | if idx_reg >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                self.emit_insn(&Insn::raw_bytes(&[rex_i, 0x69, 0xC0 | ((idx_reg & 7) << 3) | (idx_reg & 7)]));
                self.emit_insn(&Insn::raw_bytes(&16i32.to_le_bytes()));
                (0u8, (0u8 << 6) | ((idx_reg & 7) << 3) | 3)
            } else {
                (scale, sib)
            };
            let _ = scale2;
            let mut rex = 0x41u8; // REX.B: SIB base=r11
            if idx_reg >= 8 {
                rex |= 0x02; // REX.X
            }
            self.emit_seg(seg);
            self.emit_insn(&Insn::raw_bytes(&[rex, opcode, 0x04 | ((rf & 7) << 3), sib2]));
        } else if is_float {
            let xmm = 0u8; // 浮点表达式结果约定在 xmm0
            let mut rex = 0x41u8; // REX.B: SIB base=r11 (REX 前缀必须 0x40-0x4F)
            if xmm >= 8 {
                rex |= 0x04;
            }
            if idx_reg >= 8 {
                rex |= 0x02;
            }
            let mut ins = Vec::new();
            // 前缀顺序: F3 (legacy) 必须在 REX 之前, 否则 REX 字节被当成指令
            ins.push(0xF3);
            if rex != 0 {
                ins.push(rex);
            }
            ins.extend_from_slice(&[0x0F, 0x10, modrm | ((xmm & 7) << 3), sib]);
            self.emit_seg(seg);
            self.emit_insn(&Insn::raw_bytes(&ins));
        } else if is_double {
            let xmm = 0u8;
            let mut rex = 0x41u8; // REX.B: SIB base=r11 (REX 前缀必须 0x40-0x4F)
            if xmm >= 8 {
                rex |= 0x04;
            }
            if idx_reg >= 8 {
                rex |= 0x02;
            }
            let mut ins = Vec::new();
            // 前缀顺序: F2 (legacy) 必须在 REX 之前, 否则 REX 字节被当成指令
            ins.push(0xF2);
            if rex != 0 {
                ins.push(rex);
            }
            ins.extend_from_slice(&[0x0F, 0x10, modrm | ((xmm & 7) << 3), sib]);
            self.emit_seg(seg);
            self.emit_insn(&Insn::raw_bytes(&ins));
        } else {
            // 按 elem_size 和符号性选加载指令:
            // signed: movsbq/movswq/movsxd (符号扩展)
            // unsigned: movzbq/movzwq/mov (零扩展)
            let is_signed = Self::is_signed_int(elem_ty);
            let mut rex = 0x48u8 | 0x01; // REX.W + REX.B (SIB base=r11)
            if dst_gp >= 8 {
                rex |= 0x04; // REX.R
            }
            if idx_reg >= 8 {
                rex |= 0x02; // REX.X
            }
            if is_signed {
                // 符号扩展加载: 保留 REX.W
                self.emit_seg(seg);
                match elem_size {
                    1 => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBE, modrm | ((dst_gp & 7) << 3), sib])); } // movsbq
                    2 => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xBF, modrm | ((dst_gp & 7) << 3), sib])); } // movswq
                    4 => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x63, modrm | ((dst_gp & 7) << 3), sib])); }       // movsxd
                    _ => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, modrm | ((dst_gp & 7) << 3), sib])); } // mov 64-bit
                }
            } else {
                // 零扩展加载: 去掉 REX.W
                if elem_size != 8 {
                    rex &= !0x08;
                }
                self.emit_seg(seg);
                match elem_size {
                    1 => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xB6, modrm | ((dst_gp & 7) << 3), sib])); } // movzbq
                    2 => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, 0xB7, modrm | ((dst_gp & 7) << 3), sib])); } // movzwq
                    _ => { let _ = self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, modrm | ((dst_gp & 7) << 3), sib])); } // mov 32/64
                }
            }
        }
    }
    pub(crate) fn emit_load_mem_typed_disp(&mut self, size: usize, ty: &Type, disp: i64) {
        let (modrm, has_disp) = if disp == 0 {
            (0x00u8, false)
        } else if (-128..=127).contains(&disp) {
            (0x40u8, true)
        } else {
            (0x80u8, true)
        };
        match ty {
            Type::Float | Type::Double | Type::LongDouble => {
                // long double 走 `fld tbyte`（见 emit_load_scalar_float_at），
                // 旧写法把 LongDouble 归入 movsd 分支依赖“高 8 字节为 0”的旧不变量。
                // `modrm` 已按 disp 选好 mod 且 reg 域为 0，x87 只需把 reg 域换 /0 /5。
                if self.float_in_st0(ty) {
                    let (opcode, rf) = match ty {
                        Type::Float => (0xD9u8, 0u8),
                        Type::LongDouble => (0xDBu8, 5u8),
                        _ => (0xDDu8, 0u8),
                    };
                    self.emit_insn(&Insn::raw_bytes(&[opcode, (rf << 3) | modrm]));
                } else {
                    let pfx: u8 = if matches!(ty, Type::Float) { 0xF3 } else { 0xF2 };
                    self.emit_insn(&Insn::raw_bytes(&[pfx, 0x0F, 0x10, modrm]));
                }
            },
            _ => {
                if size < 8 && Self::is_signed_int(ty) {
                    match size {
                        1 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xBE, modrm])); },
                        2 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xBF, modrm])); },
                        _ => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x63, modrm])); },
                    }
                } else {
                    match size {
                        1 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xB6, modrm])); },
                        2 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x0F, 0xB7, modrm])); },
                        4 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x8B, modrm])); },
                        _ => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, modrm])); },
                    }
                }
            },
        }
        if has_disp {
            if (-128..=127).contains(&disp) {
                self.emit_insn(&Insn::raw_bytes(&[disp as u8]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&disp.to_le_bytes()));
            }
        }
    }
    pub(crate) fn emit_load_mem_typed_seg(&mut self, size: usize, ty: &Type, seg_prefix: u8) {
        self.emit_insn(&Insn::raw_bytes(&[seg_prefix]));
        self.emit_load_mem_typed(size, ty);
    }
    /// 段限定（`__seg_gs`/`__seg_fs`）目标上的聚合拷贝：`rsi` = 源，`rdi` = 目标。
    ///
    /// 为什么不能沿用 `rep movsb`：64 位模式下字符串指令的段前缀被 CPU 忽略，
    /// 所以上面挂一枚 `65` 的 `rep movsb` **等于写到非段内存** —— 不报错、不改字节，
    /// 只是写错地方（旧实现把前缀发在 `cld` 前面，连“给 movsb 用”都说不上）。
    /// gcc 同一形状发的是带前缀的普通存取（`movaps %xmm0,%gs:(%rdi)` 等）。
    ///
    /// 这里按 8/4/2/1 字节展开搬运，**每一笔落点单独带前缀**（前缀只作用下一条指令）。
    /// 尺寸是编译期常量（VLA + 段限定的聚合不在本路径：上面 `size()` 对 VLA 不可信）。
    pub(crate) fn copy_agg_to_seg(&mut self, size: usize, seg: u8) {
        // rax 在本路径上是存活值（聚合赋值的地址/值约定），当暂存器用前先压后复。
        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Push, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // push rax
        // `mod_ << 6 | rm`：rsi=110 / rdi=111 都不需要 SIB；reg 域固定 000（rax/al）。
        let disp_bytes = |me: &mut Self, mod_: u8, off: usize| {
            if mod_ == 1 {
                me.emit(&[off as u8]);
            } else if mod_ == 2 {
                me.emit(&(off as i32).to_le_bytes());
            }
        };
        // 一笔「读 [rsi+off] 的 w 字节」：rax 作暂存（源侧是通用地址空间，不需前缀）
        macro_rules! load_at {
            ($w:expr, $off:expr) => {{
                let (mod_, need_disp) = if $off == 0 {
                    (0u8, false)
                } else if $off < 128 {
                    (1u8, true)
                } else {
                    (2u8, true)
                };
                match $w {
                    8 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x8B, (mod_ << 6) | 0x06])); },
                    4 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x8B, (mod_ << 6) | 0x06])); },
                    2 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x66, 0x8B, (mod_ << 6) | 0x06])); },
                    _ => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x8A, (mod_ << 6) | 0x06])); },
                }
                if need_disp {
                    disp_bytes(self, mod_, $off);
                }
            }};
        }
        // 一笔「写 al/rax 到 seg:[rdi+off]」—— 前缀逐笔发，不共享
        macro_rules! store_at {
            ($w:expr, $off:expr) => {{
                let (mod_, need_disp) = if $off == 0 {
                    (0u8, false)
                } else if $off < 128 {
                    (1u8, true)
                } else {
                    (2u8, true)
                };
                self.emit_insn(&Insn::raw_bytes(&[seg]));
                match $w {
                    8 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x48, 0x89, (mod_ << 6) | 0x07])); },
                    4 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x89, (mod_ << 6) | 0x07])); },
                    2 => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x66, 0x89, (mod_ << 6) | 0x07])); },
                    _ => { let _ = self.emit_insn(&Insn::raw_bytes(&[0x88, (mod_ << 6) | 0x07])); },
                }
                if need_disp {
                    disp_bytes(self, mod_, $off);
                }
            }};
        }
        let mut off = 0usize;
        while size - off >= 8 {
            load_at!(8, off);
            store_at!(8, off);
            off += 8;
        }
        for w in [4usize, 2, 1] {
            if size - off >= w {
                load_at!(w, off);
                store_at!(w, off);
                off += w;
            }
        }
        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Stack { op: crate::codegen::x86_64::opcode::StackOp::Pop, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // pop rax
    }
    /// 尺寸只有运行期才能定（VLA 聚合）且目标带段时的逐字节搬运：
    /// `rsi` = 源、`rdi` = 目标、`rcx` = 己计好的字节数（与 `rep movsb` 同一约定）。
    /// 循环体定长，所以 `jnz` 的反向位移是编译期常量：不能拿运行期长度算。
    pub(crate) fn copy_bytes_to_seg_runtime(&mut self, seg: u8) {
        // 位移算术（循环体定长 16 字节）：
        //   0: 48 85 C9  test rcx,rcx
        //   3: 74 10     jz   +16  （跳到 jnz 之后，即 21）
        //   5: 8A 06     mov  al,[rsi]
        //   7: <seg> 88 07  mov  seg:[rdi],al
        //  10: 48 FF C6  inc  rsi
        //  13: 48 FF C7  inc  rdi
        //  16: 48 FF C9  dec  rcx
        //  19: 75 F0     jnz  -16  （回到 5）
        self.emit_insn(&Insn::raw_bytes(&[0x48, 0x85, 0xC9, 0x74, 0x10]));
        self.emit_insn(&Insn::raw_bytes(&[0x8A, 0x06])); // mov al, [rsi]
        self.emit_insn(&Insn::raw_bytes(&[seg, 0x88, 0x07])); // mov seg:[rdi], al
        // P11 Bucket M: inc rsi/rdi + dec rcx via UnaryRm typed opcode.
        self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Inc, crate::codegen::shared::asm_operand::Width::B64, 6));
        self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Inc, crate::codegen::shared::asm_operand::Width::B64, 7));
        self.emit_insn(&Insn::unary_rm_reg(crate::codegen::x86_64::opcode::UnaryOp::Dec, crate::codegen::shared::asm_operand::Width::B64, 1));
        self.emit_insn(&Insn::raw_bytes(&[0x75, 0xF0])); // jnz -> 回到 mov al,[rsi]
    }
    pub(crate) fn emit_rsp_modrm_disp(&mut self, reg_field: u8, off: i64) {
        if off == 0 {
            self.emit_insn(&Insn::raw_bytes(&[0x04 | ((reg_field & 7) << 3), 0x24]));
        } else if Self::fits_disp8(off) {
            self.emit_insn(&Insn::raw_bytes(&[0x44 | ((reg_field & 7) << 3), 0x24, off as i8 as u8]));
        } else {
            self.emit_insn(&Insn::raw_bytes(&[0x84 | ((reg_field & 7) << 3), 0x24]));
            self.emit_insn(&Insn::raw_bytes(&(off as i32).to_le_bytes()));
        }
    }
    pub(crate) fn emit_store_rsp0(&mut self, inner_sz: i64) {
        match inner_sz {
            8 => { let _ = self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            )); },
            4 => { let _ = self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(4),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            )); },
            2 => { let _ = self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B16,
                0, 4, 0,
            )); },
            _ => { let _ = self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B8,
                0, 4, 0,
            )); },
        }
    }
    pub(crate) fn emit_store_rspi(&mut self, inner_sz: i64) {
        match inner_sz {
            8 => { let _ = self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B64,
                0, 4, inner_sz as i32,
            )); },
            4 => { let _ = self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B32,
                0, 4, inner_sz as i32,
            )); },
            2 => { let _ = self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B16,
                0, 4, inner_sz as i32,
            )); },
            _ => { let _ = self.emit_insn(&Insn::mov_rm_mem(
                false,
                crate::codegen::shared::asm_operand::Width::B8,
                0, 4, inner_sz as i32,
            )); },
        }
    }
    pub(crate) fn lookup_global_info(&self, source_name: &str) -> Option<&GlobalVar> {
        let key = self.resolve_global_key(source_name);
        self.globals_raw.get(key)
    }
    pub(crate) fn materialize_address<'a>(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        action: crate::semantics::MaterializeAction,
        _index_expr: Option<&'a Expr>,
        original_expr: &'a Expr,
        _arr_expr: Option<&'a Expr>,
        dst: u8,
    ) -> bool {
        use crate::semantics::MaterializeAction as MA;

        // 偏移项 (`ByteOffset::Runtime.terms` / `ByteOffset::VlaRuntime.index`)
        // 自带各层下标表达式 —— 描述符完全自描述, 调用点传来的本层 `idx`
        // 不再参与寻址。旧实现在这里用 `runtime_index_in_chain` 猜「本层 idx
        // 是否就是那个运行时下标」, 只能修好单层错配, ≥三层下标与常量/变量
        // 交错时仍会算错地址 (全局多维数组行 stride 塌缩)。
        // 无法表达的形状 (`ByteOffset::Unsupported`) 统一返回 false, 由各调用
        // 点自己的旧路径接管 —— 单一真源在描述符, 不在调用点的浅层试探。
        let addr_ok = self.emit_address_descriptor_with_index(desc, original_expr, dst);
        if !addr_ok {
            return false;
        }

        // Step 2: 根据动作执行加载/存储
        let elem_size = desc.element_info.size;
        match action {
            MA::TakeAddress | MA::ArrayDecay | MA::AggregateLoad | MA::AggregateStore => {
                // 地址就是结果, 已经在 dst 中
            },
            MA::LoadScalar => {
                // 从地址加载标量值
                if dst != 0 {
                    // 地址在 dst, 需要移到 rax 再加载
                    self.push(dst);
                    self.pop(0); // rax = addr
                }
                let elem_ty = original_expr.ty();
                // 基址是 `__seg_gs`/`__seg_fs` 指针时（`p[i]`、`q->a`、`a[i].f`）必须
                // 带段前缀；判据单一入口见 [`Codegen::lvalue_seg_prefix`]。
                if let Some(seg) = self.lvalue_seg_prefix(original_expr) {
                    self.emit_insn(&Insn::raw_bytes(&[seg]));
                }
                self.emit_load_mem_typed(elem_size, &elem_ty);
                if dst != 0 && !self.is_scalar_float_channel_ty(&elem_ty) {
                    self.mov(dst, 0);
                }
            },
            MA::StoreScalar => {
                // 存储标量值到地址
                // 值在 rax, 地址在 dst; 需要地址在 rcx, 值在 rax
                if dst != 1 {
                    // 把地址移到 rcx
                    if dst == 0 {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                    } else {
                        self.push(dst);
                        self.pop(1); // rcx = addr
                    }
                }
                let elem_ty = original_expr.ty();
                // 同 LoadScalar：地址已经算在 rcx，段限定基址必须让这一笔写入落在段里。
                let seg = self.lvalue_seg_prefix(original_expr);
                if self.is_scalar_float_channel_ty(&elem_ty) {
                    // 单一真源：值可能在 xmm0（f32/f64）或 st(0)（long double），
                    // 旧写法无条件 `movsd [rcx], xmm0` 会把陈旧 xmm0 写进
                    // long double 对象，并把真正的 80-bit 值漏在 x87 栈上。
                    if let Some(seg) = seg {
                        self.emit_insn(&Insn::raw_bytes(&[seg]));
                    }
                    self.emit_store_scalar_float_at(&elem_ty, &elem_ty, 1);
                } else {
                    if let Some(seg) = seg {
                        self.emit_insn(&Insn::raw_bytes(&[seg]));
                    }
                    match elem_size {
                        1 => { let _ = self.emit_insn(&Insn::raw_with_gas(&[0x88, 0x01], "mov %al, (%rcx)")); },
                        2 => { let _ = self.emit_insn(&Insn::raw_with_gas(&[0x66, 0x89, 0x01], "mov %ax, (%rcx)")); },
                        4 => { let _ = self.emit_insn(&Insn::raw_with_gas(&[0x89, 0x01], "mov %eax, (%rcx)")); },
                        _ => { let _ = self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x89, 0x01], "mov %rax, (%rcx)")); },
                    }
                }
            },
            MA::ComplexLoad => {
                // 复数加载: 地址在 dst, 加载到临时槽并返回地址
                if dst != 0 {
                    // 地址在 dst, 需要移到 rax
                    self.push(dst);
                    self.pop(0); // rax = addr
                }
                let elem_ty = original_expr.ty();
                let result_off = self.complex_result_off();
                if let Type::Complex(ref inner) = elem_ty {
                    let inner_sz = inner.base.size();
                    let pfx = if matches!(inner.base, Type::Float) {
                        0xF3u8
                    } else {
                        0xF2u8
                    };
                    let base_off = result_off;
                    // 加载实部: [rax] → xmm0
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax
                    if matches!(inner.base, Type::Float) {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 6, 0)); // movss xmm0, [rsi]
                    } else {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 6, 0)); // movsd xmm0, [rsi]
                    }
                    // 保存实部 → [r13 + base_off]
                    self.emit_lea_r13(7, base_off);
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], base_off);
                    // 加载虚部: [rax + inner_sz] → xmm0
                    let inner_sz_i64 = inner_sz as i64;
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xc0, 0x00])); // add rax, inner_sz
                    self.emit_insn(&Insn::raw_bytes(&inner_sz_i64.to_le_bytes()));
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rsi, rax
                    if matches!(inner.base, Type::Float) {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 6, 0)); // movss xmm0, [rsi]
                    } else {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 6, 0)); // movsd xmm0, [rsi]
                    }
                    // 保存虚部 → [r13 + base_off + inner_sz]
                    self.emit_lea_r13(7, base_off + inner_sz_i64);
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], base_off + inner_sz_i64);
                    // 恢复 xmm0 到实部
                    self.emit_lea_r13(6, base_off);
                    self.emit_insn(&Insn::raw_bytes(&[pfx, 0x41, 0x0F, 0x10, 0xf0])); // movss xmm0, [r13 + off]
                                                               // 返回地址在 rax/dst
                    self.emit_lea_r13(0, base_off);
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                }
            },
            MA::ComplexStore => {
                // 复数存储: 地址在 dst, 值来自 rvalue 的临时槽
                // 值已经在内存的临时槽，只需要拷贝到目标地址
                if dst != 1 {
                    // 把地址移到 rcx
                    if dst == 0 {
                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(1, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rcx, rax
                    } else {
                        self.push(dst);
                        self.pop(1); // rcx = addr
                    }
                }
                let elem_ty = original_expr.ty();
                if let Type::Complex(ref inner) = elem_ty {
                    // rvalue 已经在 result_off 生成，直接拷贝
                    let inner_sz = inner.base.size();
                    let result_off = self.complex_result_off();
                    self.emit_lea_r13(6, result_off); // rsi = source
                                                      // 存储实部: xmm0 → [rcx]
                    self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovRm { width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(6, crate::codegen::shared::asm_operand::Width::B64)}, &[])); // mov rdx, rsi
                    if matches!(inner.base, Type::Float) {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 2, 0)); // movss xmm0, [rdx]
                    } else {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 2, 0)); // movsd xmm0, [rdx]
                    }
                    if matches!(inner.base, Type::Float) {
                        self.emit_insn(&Insn::simd_mov_mem_base(true, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 1, 0)); // movss [rcx], xmm0
                    } else {
                        self.emit_insn(&Insn::simd_mov_mem_base(true, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 1, 0)); // movsd [rcx], xmm0
                    }
                    // 存储虚部: [rdx + inner_sz] → [rcx + inner_sz]
                    let inner_sz_i64 = inner_sz as i64;
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xc2, 0x00])); // add rdx, inner_sz
                    self.emit_insn(&Insn::raw_bytes(&inner_sz_i64.to_le_bytes()));
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x81, 0xc1, 0x00])); // add rcx, inner_sz
                    self.emit_insn(&Insn::raw_bytes(&inner_sz_i64.to_le_bytes()));
                    if matches!(inner.base, Type::Float) {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 2, 0)); // movss xmm0, [rdx]
                    } else {
                        self.emit_insn(&Insn::simd_mov_mem_base(false, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 2, 0)); // movsd xmm0, [rdx]
                    }
                    if matches!(inner.base, Type::Float) {
                        self.emit_insn(&Insn::simd_mov_mem_base(true, crate::codegen::x86_64::opcode::SimdWidth::SS, 0, 1, 0)); // movss [rcx], xmm0
                    } else {
                        self.emit_insn(&Insn::simd_mov_mem_base(true, crate::codegen::x86_64::opcode::SimdWidth::SD, 0, 1, 0)); // movsd [rcx], xmm0
                    }
                }
            },
        }

        true
    }
    /// 变量存取宽度口径的**唯一真源**：返回 `(字节数, 4 字节有符号?, 窄有符号枚举?, 是聚合?)`。
    ///
    /// 为什么要把这段计算抽出来而不是各写一遍：栈槽路径的宽度规则是专门修过的（窄写
    /// 只写那么多字节、读时按符号性 `movsxd`/`movzx`），而 `register asm` 绑定路径原来是
    /// 两次裸 64 位 `mov` —— 同一个变量在两条通路下给出两个值。实测
    /// `register unsigned int v asm("rbx"); v = 0x100000001UL; unsigned long back = v;`
    /// 在 -O0 读出 `back=4294967297`，而栈槽路径与 gcc 都是 `1`。
    ///
    /// 类型来源:`var_map`（局部变量，`pre_register_vars` 必然登记过）→ 绑定表里携带的
    /// 声明类型（file-scope 的 `register asm` 变量在收集全局时被 `continue` 掉，既不在
    /// `var_map` 也不在 `globals_raw`）→ 8 字节有符号回退。
    fn var_access_shape(&self, name: &str) -> (usize, bool, bool, bool) {
        let Some(t) = self
            .var_map
            .get(name)
            .map(|(_, t)| t)
            .or_else(|| self.homes.get(name).map(|(_, t)| t))
        else {
            return (8, true, false, false);
        };
        let s = t.size();
        // 符号性: int / 默认枚举 / _Atomic(int) 视为 signed;
        // C23 固定底层类型枚举按底层类型判定 (如 enum E : int),
        // 否则负枚举值会被零扩展成巨大正数.
        let signed = |ty: &Type| -> bool {
            match ty {
                Type::Int => true,
                Type::Enum(_, _, None) => true,
                Type::Enum(_, _, Some(u)) => Self::is_signed_int(&u.base),
                Type::Atomic(b) => match &b.base {
                    Type::Int => true,
                    Type::Enum(_, _, Some(u)) => Self::is_signed_int(&u.base),
                    _ => false,
                },
                _ => false,
            }
        };
        // signed_narrow: 窄类型 (1/2 字节) 且 signed, 但调用方的
        // gen_int_promote 不处理枚举 (ty.is_int() 不含 Enum),
        // 需在此处补 movsx 符号扩展.
        let sn = s < 4 && matches!(t, Type::Enum(_, _, Some(u)) if Self::is_signed_int(&u.base));
        // For struct/union types, treat as unsigned to avoid
        // sign-extension loads that truncate the value.
        let is_struct = matches!(t, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _));
        (s, signed(t) && !is_struct, sn, is_struct)
    }
    /// 名字 → 家：**唯一的读入口**（P12 Phase 4.3，取代「先查 `var_map` 再查 `homes`」
    /// 这种在每个消费点各自定优先级的写法）。
    ///
    /// 双轨的害不在「有两条路」，而在**每个点自己决定谁优先**——§7.15/§7.19 的实测结论
    /// 是「所有权判据一旦有两套，弱的那一套决定产物」。现在优先级只在本函数里定一次，
    /// 消费点只能 `match` 到底（不留 `_ => {}`，新增变体会在这里编不过/立刻报内部错误）。
    ///
    /// 没有家的名字返回 `Stack(off)`，偏移由 `var_offset` 按今天的懒注册口径给出（同一
    /// 个名字永远得到同一个偏移），所以这一步对产物没有任何改变；把偏移在发射前全部
    /// 定完是 Phase 2.2 登记的「会长的那一段」（晚分配搬进解算阶段）的事。
    pub(crate) fn home_for(&mut self, name: &str) -> crate::location::Location {
        // 程序点级覆盖优先（前置二）：在一个 call 序列的存/取窗口里，某个寄存器家的值
        // 暂时只在内存里。读入口只有这一个，所以地址类消费点与存取两侧看到的是同一个
        // 答案；把这条判定再抄到各消费点就是第六个现场推算位置的人。
        if let Some(&loc) = self.home_overrides.get(name) {
            return loc;
        }
        if let Some((loc, _)) = self.homes.get(name) {
            return *loc;
        }
        let o = self.var_offset(name);
        crate::location::Location::Stack(o)
    }

    /// 后端还没接线的家（`Imm`、非 GP 通道）。今天没有任何生产者会给出它们，所以这里
    /// 既不 `unreachable!` 也不静默回退：**报内部错误并退回该名字的栈槽**，让「新增了
    /// 没接线的家」在测试与产物上立刻可见（同 §7.15 那族错的处理方式：宁可可见地失败）。
    ///
    /// `Spill` 不在这条路上：它从 Phase 5.1.c 起是**已接线**的家，翻译只发生在
    /// [`crate::codegen::x86_64::abi::layout::LayoutMap::spill_off`]（见 `Location::Spill` 上的
    /// 单位约定：携带的是槽序号）。
    fn unplumbed_home(&mut self, site: &str, loc: &crate::location::Location, name: &str) -> i64 {
        self.report_internal_bug(
            DIAG_UNPLUMBED_HOME,
            format!("{} 读到尚未接线的家 {:?}（变量 '{}'）", site, loc, name),
            "接多通道（XMM/x87）或常量家时必须同时更新本文件的消费点，不得让新家静默走回退",
        );
        self.var_offset(name)
    }

    /// 取地址类消费点（`AddrOf` / `MemberAddr` / `IndexAddr` 与地址计算）用的帧内偏移。
    ///
    /// 它走同一个 `home_for` 读入口，所以「先查 `var_map` 再查绑定表」的优先级在全后端
    /// 只有 `home_for` 一处。名字如果带着**非栈**的家（住在寄存器里），就是「地址逸出的
    /// 对象不得绑寄存器」那条判据漏了 —— 今天由 `register_binding_allowed` 与
    /// `bindable_ty` 挡着，但「现在挡着」不等于「不可能」；而静默把 `&v` 指向一个
    /// 从未写过的栈槽，正是 §7.15/§7.19 已经实测过的那族错值。所以这里报内部错误，
    /// 不拿 `debug_assert` 充数（release 构建里 debug_assert 不成立就仍是静默错值）。
    pub(crate) fn stack_home_off(&mut self, name: &str) -> i64 {
        match self.home_for(name) {
            crate::location::Location::Stack(o) => o,
            // spill 槽就是内存：取地址与变量槽同形（地址只在帧内有效，跳帧逸出由
            // `register_binding_allowed`/`collect_addr_escaped` 挡在外面）。
            crate::location::Location::Spill(slot) => self.frame_layout.spill_off(slot),
            other => self.unplumbed_home("取地址", &other, name),
        }
    }

    pub(crate) fn mov_from_stack(&mut self, reg: u8, name: &str) {
        let (sz, signed4, signed_narrow, is_struct) = self.var_access_shape(name);
        // 家只有一个读入口（`home_for`）；下面每个分支只回答「这个家怎么读」。
        // 寄存器路的宽度口径与栈槽路同源：写入侧立的是规范形态（低位有效、高位为 0），
        // 所以无符号与 8 字节就是普通 64 位搬运，有符号窄类型补 movsx/movsxd。
        // 先取到一个局部再匹配：诊断臂需要把同一个家按引用递出去，而在 match 的 scrutinee
        // 上再调一次 `home_for` 就是多一个读入口（旧代码正是这么写的）。
        let home = self.home_for(name);
        let off = match home {
            crate::location::Location::Reg(r) => {
                if r.bank == crate::location::RegBank::Gp {
                    self.mov_reg_extend(reg, r.n, sz, signed4 || signed_narrow);
                    return;
                }
                self.unplumbed_home("mov_from_stack", &home, name)
            }
            crate::location::Location::Stack(o) => o,
            crate::location::Location::Spill(slot) => self.frame_layout.spill_off(slot),
            crate::location::Location::Imm(_) => self.unplumbed_home("mov_from_stack", &home, name),
        };
        // 按声明类型宽度加载. 槽可能被窄写污染 (如 *ip = v 只写 4 字节,
        // 高 4 字节是启动代码残留的栈垃圾), 恒用 64 位 load 会把未定义
        // 高位一起读入 (64 位比较/传参即出错).
        // signed 4 字节用 movsxd 符号扩展, unsigned 4 字节用 32 位 load
        // (写 32 位寄存器自动清零高位); 1/2 字节用 movzx, 普通窄类型的
        // 符号语义由调用方的 gen_int_promote 处理, 窄有符号枚举在此处
        // 补 movsx (promote 不覆盖枚举); 8 字节维持 64 位 load.
        // For struct/union types, always load 8 bytes to avoid truncation.
        // Structs may have sizes like 6 bytes, but we need to load the full
        // 8-byte register to preserve all fields.
        if is_struct {
            self.load_stack(reg, off);
            return;
        }
        match sz {
            4 => {
                let rex_r = if reg >= 8 { 0x04 } else { 0 };
                let modrm = 0x45 | ((reg & 7) << 3); // mod=01, r/m=%r13(5)
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let reg_name = GAS_REG64[(reg & 15) as usize];
                let (bytes, is_signed) = if signed4 {
                    // movsxd r64, dword [r13+off] (REX.W + 0x63 /r)
                    let mut b: Vec<u8> = vec![0x49 | rex_r, 0x63];
                    if off.abs() < 128 {
                        b.push(modrm);
                        b.push(off as i8 as u8);
                    } else {
                        b.push(0x85 | ((reg & 7) << 3));
                        b.extend_from_slice(&(off as i32).to_le_bytes());
                    }
                    (b, true)
                } else {
                    // mov r32, dword [r13+off] (无 REX.W: 高 32 位清零)
                    let mut b: Vec<u8> = vec![0x41 | rex_r, 0x8B];
                    if off.abs() < 128 {
                        b.push(modrm);
                        b.push(off as i8 as u8);
                    } else {
                        b.push(0x85 | ((reg & 7) << 3));
                        b.extend_from_slice(&(off as i32).to_le_bytes());
                    }
                    (b, false)
                };
                let gas = if is_signed {
                    format!("movsxd {}(%r13), {}", off, reg_name)
                } else {
                    // 32 位形式: %eax/%ecx/.../%r8d..%r15d
                    let r32 = match reg & 15 {
                        0 => "%eax", 1 => "%ecx", 2 => "%edx", 3 => "%ebx",
                        4 => "%esp", 5 => "%ebp", 6 => "%esi", 7 => "%edi",
                        8 => "%r8d", 9 => "%r9d", 10 => "%r10d", 11 => "%r11d",
                        12 => "%r12d", 13 => "%r13d", 14 => "%r14d", 15 => "%r15d",
                        _ => "%eax",
                    };
                    format!("mov {}, {}(%r13)", r32, off)
                };
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            1 | 2 => {
                self.load_stack_sized(reg, off, sz);
                if signed_narrow {
                    // movsx reg, reg_lowN: 窄有符号枚举 (C23 enum E : signed char/short)
                    // 零扩展读后符号扩展到 64 位 (调用方 promote 不覆盖枚举)
                    let rex = 0x48 | if reg >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    let op = if sz == 1 { 0xBE } else { 0xBF };
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, op, 0xC0 | ((reg & 7) << 3) | (reg & 7)]));
                }
            },
            _ => self.load_stack(reg, off),
        }
    }
    pub(crate) fn mov_to_stack(&mut self, reg: u8, name: &str) {
        let (sz, _, _, _) = self.var_access_shape(name);
        // 与读出侧共用同一个读入口；按声明宽度截断成规范形态与下面栈槽路径的
        // 「按宽度 store」同口径（缺陷与实测见 `var_access_shape`）。
        let home = self.home_for(name);
        let off = match home {
            crate::location::Location::Reg(r) => {
                if r.bank == crate::location::RegBank::Gp {
                    self.mov_reg_truncate(r.n, reg, sz);
                    return;
                }
                self.unplumbed_home("mov_to_stack", &home, name)
            }
            crate::location::Location::Stack(o) => o,
            crate::location::Location::Spill(slot) => self.frame_layout.spill_off(slot),
            crate::location::Location::Imm(_) => self.unplumbed_home("mov_to_stack", &home, name),
        };
        // 按变量声明类型宽度存储, 避免 64 位 store 把高位垃圾
        // 写入小类型变量的栈槽 (后续 idiv 等指令加载时会读
        // 到错误的高位). 例如 `signed int xx = ulong_x` 必须只
        // 写 4 字节, 使 movsxd 加载时能正确符号扩展.
        match sz {
            1 | 2 | 4 => self.emit_store(reg, off, sz),
            _ => self.store_stack(reg, off),
        }
    }
    pub(crate) fn store_stack_imm32(&mut self, off: i64, val: i32) {
        // mov dword [r13+off], imm32
        if Self::fits_disp32(off) {
            self.emit_insn(&Insn::raw_bytes(&[0x41, 0xC7]));
            self.emit_r13_modrm_disp(0, off);
        } else {
            self.emit_lea_r13(11, off);
            self.emit_insn(&Insn::raw_bytes(&[0x41, 0xC7]));
            self.emit_reg_indirect_modrm(0, 11);
        }
        self.emit_insn(&Insn::raw_bytes(&val.to_le_bytes()));
    }
}
