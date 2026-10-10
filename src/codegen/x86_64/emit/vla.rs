//! emit/vla.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
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
    pub(crate) fn atomic_elem_size(&self, args: &[Expr], idx: usize) -> usize {
        if idx >= args.len() {
            return 8;
        }
        match args[idx].ty() {
            Type::Ptr(ref inner) => inner.size(),
            Type::Array(ref inner, _) => inner.size(),
            _ => 8,
        }
    }
    pub(crate) fn block_contains_vla(items: &[BlockItem]) -> bool {
        items.iter().any(|item| match item {
            BlockItem::Decl(Decl::Var(v)) => Self::type_has_vla(&v.ty),
            BlockItem::Decl(Decl::Func(f)) => f
                .body
                .as_ref()
                .is_some_and(|b| Self::block_contains_vla(&b.items)),
            BlockItem::Stmt(s) => Self::stmt_contains_vla(s),
            _ => false,
        })
    }
    fn emit_vla_elem_size(&mut self, ty: &Type) {
        match ty {
            Type::Array(inner, ArraySize::Var(n_expr)) => {
                self.gen_expr(n_expr, Self::EVAL_BASE); // rax = 本维元素个数
                let isz = inner.size() as i32;
                if isz != 1 {
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x69, 0xC0])); // imul rax, rax, imm32
                    self.emit_insn(&Insn::raw_bytes(&isz.to_le_bytes()));
                }
            },
            Type::Array(inner, ArraySize::Const(n)) if Self::type_has_vla(&inner.base) => {
                // 内层仍是 VLA (如 int a[2][N] 的 elem, 或 3D a[2][3][N]
                // 的 stride): 递归求内层运行时大小, 再乘本维常量个数
                self.emit_vla_elem_size(&inner.base);
                if *n != 1 {
                    self.imul_imm(0, *n as i128);
                }
            },
            _ => {
                let sz = ty.size().max(1) as i32;
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0xC7, 0xC0])); // mov rax, imm32 (sign-ext)
                self.emit_insn(&Insn::raw_bytes(&sz.to_le_bytes()));
            },
        }
    }
    pub(crate) fn emit_vla_restore(&mut self) {
        if self.cur_func_has_vla && self.frame_layout.frame_size > 0 {
            // lea rsp, [r13 + (-frame_size)]
            self.emit_lea_r13(4, self.frame_layout.frame_base());
        }
    }
    pub(crate) fn find_vla_stride_slot_from_desc(
        &self,
        desc: &crate::semantics::AddressDescriptor,
    ) -> Option<i64> {
        use crate::semantics::AddressBase;
        match &desc.base {
            AddressBase::StackSlotLoad { offset } => {
                // VLA 变量: [offset] 是数据基址槽, stride 在 offset+8
                // 需要确认这是 VLA (通过 vla_offsets 查找)
                // 这里我们用一个启发式: 检查 offset 是否在 vla_offsets 中
                for (_, &vla_off) in &self.vla_offsets {
                    if vla_off == *offset {
                        return Some(offset + 8);
                    }
                }
                None
            },
            _ => None,
        }
    }
    pub(crate) fn gen_vla_alloc(&mut self, v: &VarDecl) {
        // VM struct/union 变量 (局部类型含 VLA 字段): 整体大小是运行时
        // 值, 数据区在 rsp 动态区, 元数据槽布局与 VLA 数组统一:
        // [off]=数据基址, [off+8]=步长 (VM struct 无数组语义, 占位 0),
        // [off+16]=声明时总大小 (sizeof/聚合拷贝取用)。
        // C11 6.2.4p7: 生命周期开始时类型大小固定, 不随后续维度变量
        // 改变 (20070919-1)
        if matches!(v.ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
            && Self::type_has_vla(&v.ty)
        {
            let off = self.var_offset(&v.name.name);
            self.vla_offsets.insert(v.name.name.clone(), off);
            // rax = 声明时总大小: 声明点绑定槽优先 (gen_vm_struct_bind
            // 已在类型声明点求值), 缺失则运行时重算
            if let Some(slot) = self.vm_size_slot(&v.ty) {
                self.emit_mov_mem_r13(0, slot);
            } else {
                self.gen_sizeof_runtime(&v.ty, Self::EVAL_BASE);
            }
            self.emit_store_rax_r13(off + 16);
            self.mov_imm(0, 0i128);
            self.emit_store_rax_r13(off + 8); // stride 槽占位 0
                                              // 重新加载 size 到 rax (mov_imm 覆盖了 rax)
            self.emit_mov_mem_r13(0, off + 16);
            // 对齐 16 字节后 sub rsp 分配数据区, 基址存入 [off]
            // (与 VLA 数组同一约定, gen_lvalue_addr/load_stack 统一读)
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Add, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(15)}, &[])); // add rax, 15
            self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::And, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(-16)}, &[])); // and rax, -16
            // P11: 收敛到 AluRm{Sub, B64, rsp, rax}.
            {
                let insn = Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Sub,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            4,
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
                    self.emit_insn(&Insn::raw_bytes(&[0x48, 0x29, 0xC4])); // sub rsp, rax
                }
            }
            self.store_stack(4, off);
            return;
        }
        // VLA 变量: 最外层 ArraySize::Var, 或外层常量但内层含 Var
        // (int a[2][N] → Array(Array(Int,Var(N)), Const(2)))。两者的总
        // 大小都是运行时值, 需动态栈分配 + 3 槽元数据 (基址/行大小/
        // 声明时总大小)
        // (const_n, var_e): 最外层维度个数 — 常量或运行时表达式二选一
        let (elem_ty, const_n, var_e) = match &v.ty {
            Type::Array(elem, ArraySize::Var(se)) => (elem, 0usize, Some(se.as_ref())),
            Type::Array(elem, ArraySize::Const(n)) if Self::type_has_vla(&elem.base) => {
                (elem, *n, None)
            },
            _ => return,
        };
        // 行 (元素) 大小: elem 自身是 VLA (2D+) 时为运行时值
        self.emit_vla_elem_size(&elem_ty.base); // rax = stride
        let off = self.var_offset(&v.name.name);
        self.vla_offsets.insert(v.name.name.clone(), off);
        // mov [r13 + off + 8], rax: 存 stride 到 stride 槽
        self.emit_store_rax_r13(off + 8);
        // rax = 元素个数
        match var_e {
            Some(se) => self.gen_expr(se, Self::EVAL_BASE),
            None => self.mov_imm(0, const_n as i128),
        }
        // rax *= stride: mov rcx, [r13+off+8]; imul rax, rcx
        self.emit_mov_mem_r13(1, off + 8);
        self.emit_insn(&Insn::imul_rm_reg(
            crate::codegen::shared::asm_operand::Width::B64,
            0, 1, // rax, rcx
        ));
                                              // mov [r13 + off + 16], rax: 声明时总大小 (未对齐), sizeof 取用
        self.emit_store_rax_r13(off + 16);
        // 对齐到 16 字节: (rax + 15) & ~15
        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Add, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(15)}, &[])); // add rax, 15
        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::And, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Imm(-16)}, &[])); // and rax, -16
                                              // 保存当前 rsp (VLA 基址)
                                              // sub rsp, rax: 48 29 C4
        // P11: 收敛到 AluRm{Sub, B64, rsp, rax}.
        {
            let insn = Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::Sub,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        4,
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
                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x29, 0xC4]));
            }
        }
        self.store_stack(4, off);
        // 对齐 rsp 到 16 字节 (已经是 16 对齐的, 因为 rax 是 16 对齐的)
    }
    pub(crate) fn gen_vm_member_offset(&mut self, ty: &Type, field_name: &str, dst: u8) -> bool {
        let fields = match ty {
            Type::Struct(_, fields, _, false) => fields,
            _ => return false,
        };
        if self.vm_size_slot(ty).is_none() {
            return false;
        }
        let Some(tf_idx) = fields.iter().position(|f| f.name == field_name) else {
            return false; // 匿名穿透 (find_field extra) 场景: 交静态路径
        };
        let Some(fv_idx) = fields.iter().position(|f| f.ty.has_vla()) else {
            return false;
        };
        if fields[tf_idx].offset < fields[fv_idx].offset {
            // 目标在首个 VLA 字段之前: 静态前缀编译期正确
            return false;
        }
        // 累计器避开 dst (dst 持基址, 最后 add dst, acc 不能自加)
        let acc: u8 = if dst == 10 { 9 } else { 10 };
        // add acc, r11: opcode 01 /r, r/m=acc(B), reg=r11(R)
        // REX.W|R|B: 源 r11 需 REX.R, 累计器 r9/r10 需 REX.B
        let rex_ar = 0x4C | if acc >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
        let add_acc_r11 = [rex_ar, 0x01, 0xC0 | (3 << 3) | (acc & 7)];
        // 阶段 1: 按声明顺序求值 [fv, tf) 内各 VLA 字段 sizeof 到栈槽
        let mut slots: Vec<i64> = Vec::new();
        for f in fields.iter().take(tf_idx).skip(fv_idx) {
            if !f.ty.has_vla() {
                continue;
            }
            let slot = self.alloc_sret_slot(8);
            self.gen_sizeof_runtime(&f.ty, 11);
            self.store_stack(11, slot);
            slots.push(slot);
        }
        // 阶段 2: 纯算术累加 (无 gen_expr)
        let fv = &fields[fv_idx];
        let fa0 = fv.field_align.unwrap_or(fv.ty.align()).max(1);
        self.mov_imm(acc, ((fv.offset + fa0 - 1) & !(fa0 - 1)) as i128);
        let mut slot_iter = slots.iter();
        if tf_idx != fv_idx {
            let s0 = *slot_iter.next().unwrap();
            self.load_stack(11, s0);
            self.emit_insn(&Insn::raw_bytes(&add_acc_r11));
        }
        for f in fields
            .iter()
            .skip(fv_idx + 1)
            .take(tf_idx.saturating_sub(fv_idx + 1))
        {
            let fa = f.field_align.unwrap_or(f.ty.align()).max(1);
            if fa > 1 {
                let m = (fa - 1) as i128;
                // add/and r/m64=acc, imm32: acc>=8 需 REX.B(0x01)
                let rex = 0x48 | if acc >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let gas = format!("add ${}, {}", m as u32, GAS_REG64[(acc & 15) as usize]);
                let mut bytes = vec![rex, 0x81, 0xC0 | (acc & 7)];
                bytes.extend_from_slice(&(m as u32).to_le_bytes());
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                let gas = format!("and ${}, {}", !m as u64 as u32, GAS_REG64[(acc & 15) as usize]);
                let mut bytes = vec![rex, 0x81, 0xE0 | (acc & 7)];
                bytes.extend_from_slice(&((!m as u64 as u32).to_le_bytes()));
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            }
            if f.ty.has_vla() {
                let s = *slot_iter.next().unwrap();
                self.load_stack(11, s);
                self.emit_insn(&Insn::raw_bytes(&add_acc_r11));
            } else {
                let rex = 0x48 | if acc >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let sz = f.ty.size() as u32;
                let gas = format!("add ${}, {}", sz, GAS_REG64[(acc & 15) as usize]);
                let mut bytes = vec![rex, 0x81, 0xC0 | (acc & 7)];
                bytes.extend_from_slice(&sz.to_le_bytes());
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            }
        }
        // 目标字段偏移 = align_up(acc, tf 对齐)
        let tfa = fields[tf_idx]
            .field_align
            .unwrap_or(fields[tf_idx].ty.align())
            .max(1);
        if tfa > 1 {
            let m = (tfa - 1) as i128;
            // add/and r/m64=acc, imm32: acc>=8 需 REX.B(0x01)
            let rex = 0x48 | if acc >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
            use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
            let gas = format!("add ${}, {}", m as u32, GAS_REG64[(acc & 15) as usize]);
            let mut bytes = vec![rex, 0x81, 0xC0 | (acc & 7)];
            bytes.extend_from_slice(&(m as u32).to_le_bytes());
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            let gas = format!("and ${}, {}", !m as u64 as u32, GAS_REG64[(acc & 15) as usize]);
            let mut bytes = vec![rex, 0x81, 0xE0 | (acc & 7)];
            bytes.extend_from_slice(&((!m as u64 as u32).to_le_bytes()));
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
        }
        // dst += acc: opcode 01 /r, r/m=dst(B), reg=acc(R)
        let rex = 0x48 | if acc >= 8 { 0x04 } else { 0 } | if dst >= 8 { 0x01 } else { 0 };   // 位序: REX.B: 该寄存器只在 ModRM.r/m 或 opcode 低 3 位, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
        self.emit_insn(&Insn::raw_bytes(&[rex, 0x01, 0xC0 | ((acc & 7) << 3) | (dst & 7)]));
        true
    }
    pub(crate) fn gen_vm_struct_bind(&mut self, sd: &StructDef, is_union: bool) {
        // struct 尾部对齐: packed 布局时字段 align 全为 1, 推导值
        // 自然为 1 (无尾部圆整); 非 packed 时为最大字段对齐。
        let salign = sd
            .fields
            .iter()
            .map(|f| f.field_align.unwrap_or(f.ty.align()).max(1))
            .max()
            .unwrap_or(1);
        let vm_ty = if is_union {
            Type::Union(sd.name.name.clone(), Arc::new(sd.fields.clone()), salign, false)
        } else {
            Type::Struct(sd.name.name.clone(), Arc::new(sd.fields.clone()), salign, false)
        };
        self.gen_vm_type_bind(&vm_ty);
    }
    pub(crate) fn gen_vm_type_bind(&mut self, ty: &Type) {
        // 键拼法与「要不要占一槽」的判据都住在 `func_setup`（规划期那份静态扫描读的是
        // 同一对函数）：两处各自 `format!` / 各自扫字段，扫描就会少计一槽（`E1133`）。
        let Some(key) = Self::vm_size_key(ty) else {
            return;
        };
        if !Self::vm_record_needs_size_slot(ty) {
            self.vm_struct_size_slots.remove(&key);
            return;
        }
        let slot_name = format!("__vmsize_{}", key);
        let slot = self.register_var(&slot_name, &Type::Long);
        self.gen_sizeof_runtime(ty, Self::EVAL_BASE); // rax = 总大小 (运行时)
        self.emit_store_rax_r13(slot);
        self.vm_struct_size_slots.insert(key, slot);
    }
    fn param_vla_stride_slot(&self, name: &str) -> Option<i64> {
        self.var_map.get(name).and_then(|&(off, ref ty)| {
            if matches!(ty, Type::Ptr(q)
                if matches!(&q.base, Type::Array(_, ArraySize::Var(_))))
            {
                Some(off - 8)
            } else {
                None
            }
        })
    }
    fn stmt_contains_vla(s: &Stmt) -> bool {
        match s {
            Stmt::Block { b, .. } => Self::block_contains_vla(&b.items),
            Stmt::If { then_, else_, .. } => {
                Self::stmt_contains_vla(then_)
                    || else_.as_ref().is_some_and(|e| Self::stmt_contains_vla(e))
            },
            Stmt::While { body, .. }
            | Stmt::Do { body, .. }
            | Stmt::For { body, .. }
            | Stmt::Label { stmt: body, .. } => Self::stmt_contains_vla(body),
            Stmt::Switch { cases, .. } => cases
                .iter()
                .any(|c| Self::block_contains_vla(&c.body.items)),
            _ => false,
        }
    }
    pub(crate) fn type_has_vla(ty: &Type) -> bool {
        ty.has_vla()
    }
    pub(crate) fn vla_bound_size_slot(&self, e: &Expr) -> Option<i64> {
        match e {
            Expr::Ident { name, .. } => self.vla_offsets.get(&name.name).map(|off| off + 16),
            Expr::Index { arr, ty, .. } => {
                if !matches!(ty, Type::Array(_, ArraySize::Var(_))) {
                    return None; // 元素非 VLA: 编译期 size 即可
                }
                let mut cur = arr.as_ref();
                loop {
                    match cur {
                        Expr::Ident { name, .. } => {
                            if let Some(&off) = self.vla_offsets.get(&name.name) {
                                return Some(off + 8);
                            }
                            // VLA 形参: stride 槽在指针槽前
                            return self.param_vla_stride_slot(&name.name);
                        },
                        Expr::Index { arr: inner, .. } => cur = inner,
                        _ => return None,
                    }
                }
            },
            _ => None,
        }
    }
    pub(crate) fn vla_runtime_stride_slot(&self, arr: &Expr, elem_ty: &Type) -> Option<i64> {
        if !matches!(elem_ty, Type::Array(_, ArraySize::Var(_))) {
            return None;
        }
        let mut cur = arr;
        loop {
            match cur {
                Expr::Ident { name, .. } => {
                    if let Some(&off) = self.vla_offsets.get(&name.name) {
                        return Some(off + 8);
                    }
                    // VLA 形参: stride 槽在指针槽前
                    return self.param_vla_stride_slot(&name.name);
                },
                Expr::Index { arr: inner, .. } => cur = inner,
                _ => return None,
            }
        }
    }
    pub(crate) fn vm_size_slot(&self, ty: &Type) -> Option<i64> {
        match ty {
            Type::Struct(tag, _, _, false) => self
                .vm_struct_size_slots
                .get(&format!("struct:{}", tag))
                .copied(),
            Type::Union(tag, _, _, false) => self
                .vm_struct_size_slots
                .get(&format!("union:{}", tag))
                .copied(),
            _ => None,
        }
    }
}
