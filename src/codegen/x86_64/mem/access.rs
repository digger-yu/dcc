//! mem/access.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    pub(crate) fn copy_r13_bytes(&mut self, src_off: i64, dst_off: i64, size: usize) {
        self.emit_lea_r13(6, src_off);
        self.emit_lea_r13(7, dst_off);
        self.mov_imm(1, size as i128);
        self.emit(&[0xFC, 0xF3, 0xA4]);
    }
    fn emit_address_descriptor(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        original_expr: &Expr,
        dst: u8,
    ) -> bool {
        use crate::semantics::{AddressBase, ByteOffset};

        // 先检查是否能完整处理: 运行时偏移无法物化, 必须整体退回
        if matches!(&desc.offset, ByteOffset::Runtime { .. } | ByteOffset::VlaRuntime { .. }) {
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
            AddressBase::ExprAddr { sub } => {
                return self.emit_address_descriptor(sub, original_expr, dst);
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
                if dst != 0 {
                    let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                    self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                    self.emit(&(*off as i32).to_le_bytes());
                } else {
                    self.emit(&[0x48, 0x81, 0xC0]);
                    self.emit(&(*off as i32).to_le_bytes());
                }
            }
        }

        true
    }
    fn emit_address_descriptor_with_index(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        index_expr: &Expr,
        dst: u8,
    ) -> bool {
        use crate::semantics::{AddressBase, ByteOffset};

        // 先尝试常量偏移快速路径
        if matches!(&desc.offset, ByteOffset::Constant(_)) {
            return self.emit_address_descriptor(desc, index_expr, dst);
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
            AddressBase::ExprAddr { sub } => {
                // 递归: 子描述符也可能有运行时偏移, 但我们只处理一层
                // (两层运行时偏移需要额外临时寄存器, 暂退回 false)
                if matches!(&sub.offset, ByteOffset::Runtime { .. } | ByteOffset::VlaRuntime { .. })
                {
                    return false;
                }
                if !self.emit_address_descriptor(sub, index_expr, BASE) {
                    return false;
                }
            },
            AddressBase::PointerValue { sub } => {
                if matches!(&sub.offset, ByteOffset::Runtime { .. } | ByteOffset::VlaRuntime { .. })
                {
                    return false;
                }
                if !self.emit_address_descriptor(sub, index_expr, BASE) {
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
            ByteOffset::Runtime { stride, addend } => {
                // 保存基址, 求值索引到 rcx, 恢复基址到 rax, 计算偏移到 rcx
                self.push(BASE);
                self.gen_expr(index_expr, INDEX); // rcx = index
                self.pop(BASE); // rax = base (不能 pop 回 rcx, 否则覆盖索引)
                                // rcx *= stride
                if *stride != 1 {
                    let stride_i32 = *stride as i32;
                    self.emit(&[0x48, 0x69, 0xC9]); // imul rcx, rcx, imm32
                    self.emit(&stride_i32.to_le_bytes());
                }
                // add rcx, addend
                if *addend != 0 {
                    self.emit(&[0x48, 0x81, 0xC1]); // add rcx, imm32
                    self.emit(&(*addend as i32).to_le_bytes());
                }
                self.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx
            },
            ByteOffset::VlaRuntime { addend } => {
                // VLA: stride 在运行时确定, 需要从 VLA 元数据槽加载
                // 先找 VLA stride slot
                let stride_slot = self.find_vla_stride_slot_from_desc(desc);
                if stride_slot.is_none() {
                    return false;
                }
                let slot = stride_slot.unwrap();
                self.push(BASE);
                self.gen_expr(index_expr, INDEX); // rcx = index
                                                  // mov rdx, [r13+slot]; imul rcx, rdx
                self.emit_mov_mem_r13(2, slot);
                self.emit(&[0x48, 0x0F, 0xAF, 0xCA]); // imul rcx, rdx
                self.pop(BASE); // rax = base
                if *addend != 0 {
                    self.emit(&[0x48, 0x81, 0xC1]); // add rcx, imm32
                    self.emit(&(*addend as i32).to_le_bytes());
                }
                self.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx
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
            self.emit(&[rex_base, 0x8D, modrm, 0, 0, 0, 0]);
            let disp32_pos = self.pos() - 4;
            self.label_addr_refs.push((disp32_pos, name.to_string()));
        } else if self.defined_funcs.contains(name)
            || self.nested_funcs.contains_key(name)
            || self.target.kernel_code_model
        {
            // 内部函数或内核代码模型: LEA dst, [RIP+disp32] → 直接地址
            // 内核不使用 GOT, 所有符号在链接时直接解析
            self.emit(&[rex_base, 0x8D, modrm, 0, 0, 0, 0]);
            let disp32_pos = self.pos() - 4;
            self.pending_func_rels.push((disp32_pos, name.to_string()));
        } else {
            // 外部/未定义函数 (small model): MOV dst, [RIP+disp32] → GOT 间接
            self.emit(&[rex_base, 0x8B, modrm, 0, 0, 0, 0]);
            let disp32_pos = self.pos() - 4;
            // MOV 指令加载 GOT 条目地址 → GOTPCREL
            self.pending_func_addr_rels
                .push((disp32_pos, name.to_string(), false));
        }
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
        if rex != 0 {
            ins.push(rex);
        }
        if is_float {
            ins.extend_from_slice(&[0xF3, 0x0F, 0x10, modrm, 0, 0, 0, 0]);
        } else {
            ins.extend_from_slice(&[0xF2, 0x0F, 0x10, modrm, 0, 0, 0, 0]);
        }
        let disp_pos = self.pos() + ins.len() as u64 - 4;
        self.emit(&ins);
        // Resolve static local mangled name for relocation
        let resolved = self.resolve_global_key(name).to_string();
        // TLS 全局变量: ET_EXEC 走 pending_tls_rels 由 resolve() 修补,
        // ET_REL 走 pending_data_rels + R_X86_64_PC32 (与整数路径一致)
        let is_tls = self
            .globals_raw
            .get(&resolved)
            .map(|g| g.is_tls)
            .unwrap_or(false);
        if is_tls && !self.relocatable {
            self.pending_tls_rels.push((disp_pos, resolved.clone()));
        } else {
            self.pending_data_rels.push((disp_pos, resolved));
        }
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
                self.emit(&[0x48, 0x83, 0xC0, offset as u8]); // add rax, imm8
            } else {
                self.emit(&[0x48, 0x05]); // will use add rax, imm32
                                          // Actually: 48 05 is add rax, imm32 without ModRM
                                          // Correction: REX.W + 05 = add rax, imm32
                self.emit(&offset.to_le_bytes());
            }
        }
        // 从 [rax] 加载到 xmm
        let pfx = if is_float { 0xF3u8 } else { 0xF2u8 };
        let xmm_rex = if xmm >= 8 { 0x04u8 } else { 0u8 };
        if xmm_rex != 0 {
            self.emit(&[xmm_rex]);
        }
        self.emit(&[pfx, 0x0F, 0x10, 0x00]); // movss/movsd xmm0, [rax]
    }
    pub(crate) fn emit_global_float_store(&mut self, name: &str, ty: &Type) {
        let resolved = self.resolve_global_key(name).to_string();
        let (_, is_tls, _off) = self
            .globals_raw
            .get(&resolved)
            .map(|g| (g.size, g.is_tls, g.offset))
            .unwrap_or((8, false, 0));
        let tls_exec = is_tls && !self.relocatable;
        let is_float = matches!(ty, Type::Float);
        let pfx = if is_float { 0xF3 } else { 0xF2 };
        let pos = self.pos() + 4;
        self.emit(&[pfx, 0x0F, 0x11, 0x05, 0, 0, 0, 0]);
        if tls_exec {
            self.pending_tls_rels.push((pos, resolved.clone()));
        } else {
            self.pending_data_rels.push((pos, resolved.clone()));
        }
        if matches!(ty, Type::LongDouble) {
            // 高 8 字节写零: rax = &g; mov qword [rax+8], 0
            self.emit_global_addr(0, name);
            self.emit(&[0x48, 0xC7, 0x40, 0x08, 0, 0, 0, 0]);
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
        if rex != 0 {
            ins.push(rex);
        }
        if is_float {
            ins.extend_from_slice(&[0xF3, 0x0F, 0x11, modrm, 0, 0, 0, 0]);
        } else {
            ins.extend_from_slice(&[0xF2, 0x0F, 0x11, modrm, 0, 0, 0, 0]);
        }
        let disp_pos = self.pos() + ins.len() as u64 - 4;
        self.emit(&ins);
        self.pending_data_rels.push((disp_pos, resolved));
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
                    self.emit(&[rex, 0x8B, modrm]);
                    self.emit(&(ioff as i32).to_le_bytes());
                } else {
                    self.mov_imm(10, ioff as i128);
                    self.emit(&[0x4D, 0x8D, 0x1C, 0xD3]);
                }
            }
            if Self::type_has_vla(arr_ty) {
                // VLA: 起源帧[off] 是元数据槽, 槽内容是数据基址 (二次加载)
                // 指令: mov dst, [dst + parent_off]  (dst 已指向起源帧)
                // modrm: mod=10(disp32), reg=dst, rm=dst
                // 原 bug 用 0x03 (mod=00, rm=rbx) 导致 mov (%rbx), dst +
                // 垃圾字节被后续指令流误解析 → SIGILL.
                if Self::fits_disp32(parent_off) {
                    let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                    let modrm = 0x80 | ((dst & 7) << 3) | (dst & 7);
                    self.emit(&[rex, 0x8B, modrm]);
                    self.emit(&(parent_off as i32).to_le_bytes());
                } else {
                    self.mov_imm(10, parent_off as i128);
                    // lea dst, [dst + r10]
                    self.emit(&[0x4D, 0x8D, 0x1C, 0xD0 | (dst & 7)]);
                    // mov dst, [dst]
                    let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                    self.emit(&[rex, 0x8B, (dst & 7)]);
                }
            } else {
                // 普通数组/向量: 起源帧 + parent_off 即数据地址
                if parent_off != 0 {
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                    self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                    self.emit(&(parent_off as i32).to_le_bytes());
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
                self.emit(&[0x48, 0x0F, 0xAF, 0xCA]); // imul rcx, rdx
            } else {
                self.gen_sizeof_runtime(elem_ty, 11);
                // imul rcx, r11: REX.W+B (r11 作 r/m), modrm reg=rcx
                self.emit(&[0x49, 0x0F, 0xAF, 0xCB]);
            }
            return;
        }
        if es == 1 {
            return;
        }
        if let Some(slot) = self.vla_runtime_stride_slot(arr, elem_ty) {
            // mov rdx, [r13+slot]; imul rcx, rdx
            self.emit_mov_mem_r13(2, slot);
            self.emit(&[0x48, 0x0F, 0xAF, 0xCA]);
        } else {
            self.emit(&[0x48, 0x69, 0xC9]); // imul rcx, rcx, imm32
            self.emit(&es.to_le_bytes());
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
        let scale = match elem_size {
            2 => 1u8,
            4 => 2,
            8 => 3,
            _ => 0,
        };
        // lea r11, [rip + disp32] (RIP-relative, 链接时修补 PC 相对位移)
        let lea_pos = self.pos();
        self.emit(&[0x4C, 0x8D, 0x1D, 0, 0, 0, 0]);
        self.pending_data_rels
            .push((lea_pos + 3, global.to_string()));
        let sib = (scale << 6) | ((idx_reg & 7) << 3) | 3; // base=r11 (REX.B)
        let modrm = 0x04; // mod=00, rm=100 (SIB), reg 字段在下方按目标寄存器填充
        let is_float = matches!(elem_ty, Type::Float);
        let is_double = matches!(elem_ty, Type::Double | Type::LongDouble);
        if is_float {
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
            self.emit(&ins);
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
            self.emit(&ins);
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
                match elem_size {
                    1 => self.emit(&[rex, 0x0F, 0xBE, modrm | ((dst_gp & 7) << 3), sib]), // movsbq
                    2 => self.emit(&[rex, 0x0F, 0xBF, modrm | ((dst_gp & 7) << 3), sib]), // movswq
                    4 => self.emit(&[rex, 0x63, modrm | ((dst_gp & 7) << 3), sib]),       // movsxd
                    _ => self.emit(&[rex, 0x8B, modrm | ((dst_gp & 7) << 3), sib]), // mov 64-bit
                }
            } else {
                // 零扩展加载: 去掉 REX.W
                if elem_size != 8 {
                    rex &= !0x08;
                }
                match elem_size {
                    1 => self.emit(&[rex, 0x0F, 0xB6, modrm | ((dst_gp & 7) << 3), sib]), // movzbq
                    2 => self.emit(&[rex, 0x0F, 0xB7, modrm | ((dst_gp & 7) << 3), sib]), // movzwq
                    _ => self.emit(&[rex, 0x8B, modrm | ((dst_gp & 7) << 3), sib]), // mov 32/64
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
            Type::Float => self.emit(&[0xF3, 0x0F, 0x10, modrm]),
            Type::Double | Type::LongDouble => self.emit(&[0xF2, 0x0F, 0x10, modrm]),
            _ => {
                if size < 8 && Self::is_signed_int(ty) {
                    match size {
                        1 => self.emit(&[0x48, 0x0F, 0xBE, modrm]),
                        2 => self.emit(&[0x48, 0x0F, 0xBF, modrm]),
                        _ => self.emit(&[0x48, 0x63, modrm]),
                    }
                } else {
                    match size {
                        1 => self.emit(&[0x48, 0x0F, 0xB6, modrm]),
                        2 => self.emit(&[0x48, 0x0F, 0xB7, modrm]),
                        4 => self.emit(&[0x8B, modrm]),
                        _ => self.emit(&[0x48, 0x8B, modrm]),
                    }
                }
            },
        }
        if has_disp {
            if (-128..=127).contains(&disp) {
                self.emit(&[disp as u8]);
            } else {
                self.emit(&disp.to_le_bytes());
            }
        }
    }
    pub(crate) fn emit_load_mem_typed_seg(&mut self, size: usize, ty: &Type, seg_prefix: u8) {
        self.emit(&[seg_prefix]);
        self.emit_load_mem_typed(size, ty);
    }
    pub(crate) fn emit_rsp_modrm_disp(&mut self, reg_field: u8, off: i64) {
        if off == 0 {
            self.emit(&[0x04 | ((reg_field & 7) << 3), 0x24]);
        } else if Self::fits_disp8(off) {
            self.emit(&[0x44 | ((reg_field & 7) << 3), 0x24, off as i8 as u8]);
        } else {
            self.emit(&[0x84 | ((reg_field & 7) << 3), 0x24]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }
    pub(crate) fn emit_store_rsp0(&mut self, inner_sz: i64) {
        match inner_sz {
            8 => self.emit(&[0x48, 0x89, 0x04, 0x24]),
            4 => self.emit(&[0x89, 0x04, 0x24]),
            2 => self.emit(&[0x66, 0x89, 0x04, 0x24]),
            _ => self.emit(&[0x88, 0x04, 0x24]),
        }
    }
    pub(crate) fn emit_store_rspi(&mut self, inner_sz: i64) {
        match inner_sz {
            8 => self.emit(&[0x48, 0x89, 0x44, 0x24, inner_sz as u8]),
            4 => self.emit(&[0x89, 0x44, 0x24, inner_sz as u8]),
            2 => self.emit(&[0x66, 0x89, 0x44, 0x24, inner_sz as u8]),
            _ => self.emit(&[0x88, 0x44, 0x24, inner_sz as u8]),
        }
    }
    pub(crate) fn lookup_global_info(&self, source_name: &str) -> Option<&GlobalVar> {
        let key = self.resolve_global_key(source_name);
        self.globals_raw.get(key)
    }
    pub(crate) fn materialize_address(
        &mut self,
        desc: &crate::semantics::AddressDescriptor,
        action: crate::semantics::MaterializeAction,
        index_expr: Option<&Expr>,
        original_expr: &Expr,
        _arr_expr: Option<&Expr>,
        dst: u8,
    ) -> bool {
        use crate::semantics::MaterializeAction as MA;

        // Step 1: 计算地址到 dst (rax)
        let addr_ok = if let Some(idx) = index_expr {
            self.emit_address_descriptor_with_index(desc, idx, dst)
        } else {
            self.emit_address_descriptor(desc, original_expr, dst)
        };
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
                self.emit_load_mem_typed(elem_size, &elem_ty);
                if dst != 0 && !matches!(elem_ty, Type::Float | Type::Double) {
                    self.mov(dst, 0);
                }
            },
            MA::StoreScalar => {
                // 存储标量值到地址
                // 值在 rax, 地址在 dst; 需要地址在 rcx, 值在 rax
                if dst != 1 {
                    // 把地址移到 rcx
                    if dst == 0 {
                        self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                    } else {
                        self.push(dst);
                        self.pop(1); // rcx = addr
                    }
                }
                let elem_ty = original_expr.ty();
                if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                    // 值在 xmm0, 存储到 [rcx]
                    if matches!(elem_ty, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x11, 0x01]); // movss [rcx], xmm0
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x11, 0x01]); // movsd [rcx], xmm0
                    }
                } else {
                    match elem_size {
                        1 => self.emit(&[0x88, 0x01]),
                        2 => self.emit(&[0x66, 0x89, 0x01]),
                        4 => self.emit(&[0x89, 0x01]),
                        _ => self.emit(&[0x48, 0x89, 0x01]),
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
                let result_off = -self.frame_size + 8;
                if let Type::Complex(ref inner) = elem_ty {
                    let inner_sz = inner.base.size();
                    let pfx = if matches!(inner.base, Type::Float) {
                        0xF3u8
                    } else {
                        0xF2u8
                    };
                    let base_off = result_off;
                    // 加载实部: [rax] → xmm0
                    self.emit(&[0x48, 0x89, 0xc6]); // mov rsi, rax
                    if matches!(inner.base, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x10, 0x06]); // movss xmm0, [rsi]
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x10, 0x06]); // movsd xmm0, [rsi]
                    }
                    // 保存实部 → [r13 + base_off]
                    self.emit_lea_r13(7, base_off);
                    self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], base_off);
                    // 加载虚部: [rax + inner_sz] → xmm0
                    let inner_sz_i64 = inner_sz as i64;
                    self.emit(&[0x48, 0x81, 0xc0, 0x00]); // add rax, inner_sz
                    self.emit(&inner_sz_i64.to_le_bytes());
                    self.emit(&[0x48, 0x89, 0xc6]); // mov rsi, rax
                    if matches!(inner.base, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x10, 0x06]); // movss xmm0, [rsi]
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x10, 0x06]); // movsd xmm0, [rsi]
                    }
                    // 保存虚部 → [r13 + base_off + inner_sz]
                    self.emit_lea_r13(7, base_off + inner_sz_i64);
                    self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], base_off + inner_sz_i64);
                    // 恢复 xmm0 到实部
                    self.emit_lea_r13(6, base_off);
                    self.emit(&[pfx, 0x49, 0x0F, 0x10, 0xf0]); // movss xmm0, [r13 + off]
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
                        self.emit(&[0x48, 0x89, 0xc1]); // mov rcx, rax
                    } else {
                        self.push(dst);
                        self.pop(1); // rcx = addr
                    }
                }
                let elem_ty = original_expr.ty();
                if let Type::Complex(ref inner) = elem_ty {
                    // rvalue 已经在 result_off 生成，直接拷贝
                    let inner_sz = inner.base.size();
                    let result_off = -self.frame_size + 8;
                    self.emit_lea_r13(6, result_off); // rsi = source
                                                      // 存储实部: xmm0 → [rcx]
                    self.emit(&[0x48, 0x89, 0xf2]); // mov rdx, rsi
                    if matches!(inner.base, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x10, 0x02]); // movss xmm0, [rdx]
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x10, 0x02]); // movsd xmm0, [rdx]
                    }
                    if matches!(inner.base, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x11, 0x01]); // movss [rcx], xmm0
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x11, 0x01]); // movsd [rcx], xmm0
                    }
                    // 存储虚部: [rdx + inner_sz] → [rcx + inner_sz]
                    let inner_sz_i64 = inner_sz as i64;
                    self.emit(&[0x48, 0x81, 0xc2, 0x00]); // add rdx, inner_sz
                    self.emit(&inner_sz_i64.to_le_bytes());
                    self.emit(&[0x48, 0x81, 0xc1, 0x00]); // add rcx, inner_sz
                    self.emit(&inner_sz_i64.to_le_bytes());
                    if matches!(inner.base, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x10, 0x02]); // movss xmm0, [rdx]
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x10, 0x02]); // movsd xmm0, [rdx]
                    }
                    if matches!(inner.base, Type::Float) {
                        self.emit(&[0xF3, 0x0F, 0x11, 0x01]); // movss [rcx], xmm0
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x11, 0x01]); // movsd [rcx], xmm0
                    }
                }
            },
        }

        true
    }
    pub(crate) fn mov_from_stack(&mut self, reg: u8, name: &str) {
        // P1-5: register asm 变量直接从绑定寄存器加载
        if let Some(&bound_reg) = self.reg_bindings.get(name) {
            if reg != bound_reg {
                self.mov(reg, bound_reg);
            }
            return;
        }
        let off = self.var_offset(name);
        // 按声明类型宽度加载. 槽可能被窄写污染 (如 *ip = v 只写 4 字节,
        // 高 4 字节是启动代码残留的栈垃圾), 恒用 64 位 load 会把未定义
        // 高位一起读入 (64 位比较/传参即出错).
        // signed 4 字节用 movsxd 符号扩展, unsigned 4 字节用 32 位 load
        // (写 32 位寄存器自动清零高位); 1/2 字节用 movzx, 普通窄类型的
        // 符号语义由调用方的 gen_int_promote 处理, 窄有符号枚举在此处
        // 补 movsx (promote 不覆盖枚举); 8 字节维持 64 位 load.
        let (sz, signed4, signed_narrow) = match self.var_map.get(name) {
            Some((_, t)) => {
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
                let sn =
                    s < 4 && matches!(t, Type::Enum(_, _, Some(u)) if Self::is_signed_int(&u.base));
                // For struct/union types, treat as unsigned to avoid
                // sign-extension loads that truncate the value.
                let is_struct = matches!(t, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _));
                (s, signed(t) && !is_struct, sn)
            },
            None => (8, true, false),
        };
        // For struct/union types, always load 8 bytes to avoid truncation.
        // Structs may have sizes like 6 bytes, but we need to load the full
        // 8-byte register to preserve all fields.
        let is_struct_type = match self.var_map.get(name) {
            Some((_, t)) => matches!(t, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)),
            None => false,
        };
        if is_struct_type {
            self.load_stack(reg, off);
            return;
        }
        match sz {
            4 => {
                let rex_r = if reg >= 8 { 0x04 } else { 0 };
                if signed4 {
                    // movsxd reg, dword [r13+off] (REX.W + 0x63 /r)
                    self.emit(&[0x49 | rex_r, 0x63]);
                } else {
                    // mov reg32, dword [r13+off] (无 REX.W: 高 32 位清零)
                    self.emit(&[0x41 | rex_r, 0x8B]);
                }
                if off.abs() < 128 {
                    self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((reg & 7) << 3)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            1 | 2 => {
                self.load_stack_sized(reg, off, sz);
                if signed_narrow {
                    // movsx reg, reg_lowN: 窄有符号枚举 (C23 enum E : signed char/short)
                    // 零扩展读后符号扩展到 64 位 (调用方 promote 不覆盖枚举)
                    let rex = 0x48 | if reg >= 8 { 0x44 } else { 0 };
                    let op = if sz == 1 { 0xBE } else { 0xBF };
                    self.emit(&[rex, 0x0F, op, 0xC0 | ((reg & 7) << 3) | (reg & 7)]);
                }
            },
            _ => self.load_stack(reg, off),
        }
    }
    pub(crate) fn mov_to_stack(&mut self, reg: u8, name: &str) {
        // P1-5: register asm 变量直接存入绑定寄存器
        if let Some(&bound_reg) = self.reg_bindings.get(name) {
            if reg != bound_reg {
                self.mov(bound_reg, reg);
            }
            return;
        }
        let off = self.var_offset(name);
        // 按变量声明类型宽度存储, 避免 64 位 store 把高位垃圾
        // 写入小类型变量的栈槽 (后续 idiv 等指令加载时会读
        // 到错误的高位). 例如 `signed int xx = ulong_x` 必须只
        // 写 4 字节, 使 movsxd 加载时能正确符号扩展.
        let sz = self.var_map.get(name).map(|(_, t)| t.size()).unwrap_or(8);
        match sz {
            1 | 2 | 4 => self.emit_store(reg, off, sz),
            _ => self.store_stack(reg, off),
        }
    }
    pub(crate) fn store_stack_imm32(&mut self, off: i64, val: i32) {
        // mov dword [r13+off], imm32
        if Self::fits_disp32(off) {
            self.emit(&[0x41, 0xC7]);
            self.emit_r13_modrm_disp(0, off);
        } else {
            self.emit_lea_r13(11, off);
            self.emit(&[0x41, 0xC7]);
            self.emit_reg_indirect_modrm(0, 11);
        }
        self.emit(&val.to_le_bytes());
    }
}
