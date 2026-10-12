// src/codegen/x86_64/mem/addr.rs
// L-value address calculation and expression decay for x86_64 backend.
// 本文件由 mod.rs 逐字搬迁而来 (batch 1): 原样剪切 `gen_lvalue_addr` / `gen_expr_decay`
// 到 `impl Codegen` 块内, 语义 diff 为 0。

use crate::ast::*;
use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{MemOperand, Operand, X86_64Opcode};
use crate::codegen::x86_64::Codegen;

impl Codegen {
    /// 计算任意 lvalue 表达式的地址到 rax (dst != 0 时再 mov).
    /// 支持 Ident / Member / Index / Deref / Cast 递归组合, 例如
    /// `s.i[k]`, `p->arr[i].f`, `(*q).buf[n]`. 返回 false 表示无法处理.
    pub fn gen_lvalue_addr(&mut self, e: &Expr, dst: u8) -> bool {
        match e {
            Expr::Ident { name, ty, .. } => {
                // 嵌套函数 upvar: 对象在定义父帧中 (VLA 二次加载基址,
                // 其余取父帧+off 地址), 详见 emit_ident_base_addr 注释
                if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name) {
                    let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                    let intermediates: Vec<i64> = self
                        .upvar_intermediate_offsets
                        .get(&name.name)
                        .cloned()
                        .unwrap_or_default();
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
                                    dst: Operand::Reg(11, Width::B64),
                                    mem: MemOperand {
                                        disp: 0,
                                        base: Some(11),
                                        index: Some((10, 1)),
                                        seg: None,
                                    },
                                    rip_relative: false,
                                },
                                &[],
                            );
                            if !self.emit_insn(&insn) {
                                self.emit_insn(&Insn::raw_bytes(&[0x4F, 0x8D, 0x1C, 0xD3]));
                                // 兜底
                            }
                        }
                    }
                    if Self::type_has_vla(ty) {
                        let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                        let modrm = 0x03 | ((dst & 7) << 3);
                        self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, modrm]));
                        self.emit_insn(&Insn::raw_bytes(&(parent_off as i32).to_le_bytes()));
                    } else {
                        if parent_off != 0 {
                            // add dst, imm32: 走 alu_rm_reg_imm32_b64 (恒 0x81 id)
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, parent_off as i64));
                        }
                    }
                    return true;
                }
                if self.is_global_not_local(&name.name) {
                    self.emit_global_addr(dst, &name.name);
                } else if self.vla_offsets.contains_key(&name.name) {
                    // VLA: 基址存储在变量栈槽中
                    let off = self.stack_home_off(&name.name);
                    self.load_stack(dst, off);
                } else {
                    let off = self.stack_home_off(&name.name);
                    self.emit_lea_r13(dst, off);
                }
                true
            },
            Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } => {
                if *is_arrow {
                    self.gen_expr(expr, dst);
                } else if !self.gen_lvalue_addr(expr, dst) {
                    return false;
                }
                // 注意: `->` 时 expr.ty() 是指针类型, 须解引用后再查字段偏移
                let expr_ty = expr.ty();
                let target_ty =
                    self.resolve_complete_struct_ty(Self::member_target_ty(expr_ty, *is_arrow));
                let field_off = match &target_ty {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                        find_field(fields, &field.name).map(|(f, extra)| f.offset + extra)
                    },
                    _ => None,
                };
                // VM struct 运行时成员偏移 (目标字段在首 VLA 字段之后,
                // pr82210): 偏移已加到 dst, 跳过静态偏移加法
                let vm_off_done = matches!(&target_ty, Type::Struct(_, _, _, _))
                    && self.gen_vm_member_offset(&target_ty, &field.name, dst);
                if vm_off_done {
                    return true;
                }
                if let Some(off) = field_off {
                    let off = off as i32;
                    if off != 0 {
                        // add dst, off: 走 alu_rm_reg_imm32_b64 (恒 0x81 id)
                        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, off as i64));
                    }
                }
                true
            },
            Expr::Index {
                arr,
                idx,
                ty: elem_ty,
                ..
            } => {
                // 语义层路径: compute_lvalue_address + TakeAddress
                use crate::semantics::{compute_lvalue_address, AddressContext, MaterializeAction};

                let ctx = AddressContext {
                    is_nested: self.current_func_is_nested,
                };

                // 描述符自描述多维下标链 (`ByteOffset::Runtime.terms` 逐项
                // 携带自己的下标表达式), 旧的「嵌套运行时索引」浅层试探已
                // 删除: 无法表达的形状由 materialize_address 返回 false,
                // 落到下面旧路径。
                {
                    let desc = compute_lvalue_address(self, e, &ctx);

                    if self.materialize_address(
                        &desc,
                        MaterializeAction::TakeAddress,
                        Some(idx),
                        e,
                        Some(arr),
                        dst,
                    ) {
                        return true;
                    }
                }

                // Fallback: 旧路径
                let es = elem_ty.size().max(1) as i32;
                let arr_ty = arr.ty();
                if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                    match &**arr {
                        Expr::Ident { .. } | Expr::Index { .. } => {
                            if !self.gen_lvalue_addr(arr, Self::EVAL_BASE) {
                                self.gen_expr(arr, Self::EVAL_BASE);
                            }
                        },
                        _ => {
                            self.gen_expr(arr, Self::EVAL_BASE);
                        },
                    }
                } else {
                    self.gen_expr(arr, Self::EVAL_BASE);
                }
                self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                self.gen_expr(idx, 1); // rcx = index
                self.pop(0);
                self.emit_index_scale(es, arr, elem_ty);
                self.emit_insn(&Insn::alu_rm_reg_reg(
                    crate::codegen::x86_64::opcode::AluOp::Add,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    1,
                )); // add rax, rcx
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            },
            Expr::Deref { e: inner, .. } => {
                self.gen_expr(inner, dst);
                true
            },
            Expr::Cast { e: inner, .. } => self.gen_lvalue_addr(inner, dst),
            _ => false,
        }
    }
    // 生成表达式值, 数组标识符衰减为指针 (arr → &arr[0])
    pub fn gen_expr_decay(&mut self, e: &Expr, dst: u8) {
        if let Expr::Ident { name, ty, .. } = e {
            if matches!(ty, Type::Array(_, _)) {
                if self.is_global_not_local(&name.name) {
                    // 全局数组: RIP-relative LEA 取地址
                    self.emit_global_addr(dst, &name.name);
                } else {
                    let off = self.stack_home_off(&name.name);
                    // REX.W + REX.B (r13) + REX.R (dst >= 8 → r10/r11/...)
                    let rex = 0x48u8 | 0x01 | (if dst >= 8 { 0x04 } else { 0 });
                    use crate::codegen::x86_64::emit::gas::mem_disp as gas_mem_disp;
                    use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                    let gas = format!(
                        "lea {}, {}",
                        gas_mem_disp(13, off),
                        GAS_REG64[(dst & 15) as usize]
                    );
                    let mut bytes = vec![rex, 0x8D];
                    if off.abs() < 128 {
                        bytes.push(0x45 | ((dst & 7) << 3));
                        bytes.push(off as i8 as u8);
                    } else {
                        bytes.push(0x85 | ((dst & 7) << 3));
                        bytes.extend_from_slice(&(off as i32).to_le_bytes());
                    }
                    self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                }
                return;
            }
        }
        // C11 6.3.2.1p3: 复合数组表达式 (*p / s.arr / p[i]) 的值上下文
        // 衰减为首元素地址. gen_expr 对部分形态按数组大小加载内容,
        // 故非 Ident 数组 lvalue 先取地址.
        if matches!(e.ty(), Type::Array(_, _)) && self.gen_lvalue_addr(e, dst) {
            return;
        }
        self.gen_expr(e, dst);
    }
    /// gen_expr 取地址分支 (batch F6 逐字搬迁: Expr::AddrOf arm, 188 行)。
    /// &var / &struct.field / &ptr->field / *ptr: 计算地址到 dst。
    pub fn gen_expr_addrof(&mut self, e: &Expr, dst: u8) {
        if let Expr::AddrOf { e, .. } = e {
            // &var / &struct.field / &ptr->field / *ptr: 计算地址到 dst
            if let Expr::Ident {
                name, ty: id_ty, ..
            } = e.as_ref()
            {
                if matches!(id_ty, Type::Func(_, _)) && !self.var_map.contains_key(&name.name) {
                    // &func: 函数地址
                    // 沿作用域链解析嵌套函数引用 (支持兄弟/祖先定义)
                    if let Some((unique, hops, offs)) = self.resolve_nested_func_ref(&name.name) {
                        self.load_trampoline_addr(dst, &unique, hops, &offs);
                    } else {
                        let effective_name = self
                            .nested_func_renames
                            .get(&self.current_func_name)
                            .and_then(|m| m.get(&name.name))
                            .cloned()
                            .unwrap_or_else(|| name.name.clone());
                        self.emit_func_addr(dst, &effective_name);
                    }
                } else if self.is_global_not_local(&name.name) {
                    self.emit_global_addr(dst, &name.name);
                } else if let Some(&off) = self.vla_offsets.get(&name.name) {
                    // VLA/VM struct 变量: 取地址 = 数据区基址 (元数
                    // 据槽), 与数组名衰减语义一致 (C11 6.5.3.2: &
                    // 返回对象首地址, 非元数据槽)
                    self.load_stack(dst, off);
                } else {
                    let off = self.stack_home_off(&name.name);
                    // REX.W + REX.B (r13) + REX.R (当 dst >= 8, 编码 r10/r11/r12/...)
                    let rex = 0x48u8 | 0x01 | (if dst >= 8 { 0x04 } else { 0 });
                    use crate::codegen::x86_64::emit::gas::mem_disp as gas_mem_disp;
                    use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                    let gas = format!(
                        "lea {}, {}",
                        gas_mem_disp(13, off),
                        GAS_REG64[(dst & 15) as usize]
                    );
                    let mut bytes = vec![rex, 0x8D];
                    if off.abs() < 128 {
                        bytes.push(0x45 | ((dst & 7) << 3));
                        bytes.push(off as i8 as u8);
                    } else {
                        bytes.push(0x85 | ((dst & 7) << 3));
                        bytes.extend_from_slice(&(off as i32).to_le_bytes());
                    }
                    self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                }
            } else if let Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } = e.as_ref()
            {
                // &struct.field: 基址 + 字段偏移 (不加载值)
                if *is_arrow {
                    self.gen_expr(expr, dst);
                } else if let Expr::Ident { name, .. } = expr.as_ref() {
                    if self.is_global_not_local(&name.name) {
                        self.emit_global_addr(dst, &name.name);
                    } else {
                        let off = self.stack_home_off(&name.name);
                        // 同一件事的另一份手抄本：dst>=8 时缺 REX.R ⇒ lea 的目的被
                        // 编成低 8 号。改走单源入口 `emit_lea_r13`（它已经在算 REX.R）。
                        self.emit_lea_r13(dst, off);
                    }
                } else {
                    // 通用路径: 基址来自任意 lvalue (如 &s.a.b, &s.i[k].f)
                    if !self.gen_lvalue_addr(expr, dst) {
                        self.gen_expr(expr, dst);
                    }
                }
                // 注意: `->` 时 expr.ty() 是指针类型, 须解引用后再查字段偏移
                let expr_ty = expr.ty();
                let target_ty =
                    self.resolve_complete_struct_ty(Self::member_target_ty(expr_ty, *is_arrow));
                let field_info = match &target_ty {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                        find_field(fields, &field.name).map(|(f, extra)| {
                            let mut fc = f.clone();
                            fc.offset += extra;
                            fc
                        })
                    },
                    _ => None,
                };
                if let Some(ref f) = field_info {
                    let field_off = f.offset as i32;
                    if field_off != 0 {
                        // add dst, field_off: 走 alu_rm_reg_imm32_b64 (恒 0x81 id)
                        self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, field_off as i64));
                    }
                }
            } else if let Expr::Deref { e: inner, .. } = e.as_ref() {
                // *ptr: 地址就是指针值
                self.gen_expr(inner, dst);
            } else if let Expr::Index {
                arr,
                idx,
                ty: elem_ty,
                ..
            } = e.as_ref()
            {
                // &arr[i]: 基址 + i * elem_size
                let es = elem_ty.size() as i32;
                if let Expr::Ident {
                    name: arr_name,
                    ty: arr_ty,
                    ..
                } = arr.as_ref()
                {
                    if self.is_global_not_local(&arr_name.name) {
                        // 全局数组 → 取地址; 全局指针 → 加载值
                        if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else {
                            self.emit_global_load(0, &arr_name.name);
                        }
                    } else if self.vla_offsets.contains_key(&arr_name.name) {
                        let off = self.stack_home_off(&arr_name.name);
                        self.load_stack(0, off);
                    } else if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                        // 局部数组: LEA 取地址
                        let off = self.stack_home_off(&arr_name.name);
                        self.emit_lea_r13(0, off);
                    } else {
                        // 局部指针: 加载指针值
                        let off = self.stack_home_off(&arr_name.name);
                        self.load_stack(0, off);
                    }
                } else {
                    // 通用路径: 基址来自任意 lvalue/指针表达式 (如 &s.i[k])
                    let arr_ty = arr.ty();
                    if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                        if !self.gen_lvalue_addr(arr, Self::EVAL_BASE) {
                            self.gen_expr(arr, Self::EVAL_BASE);
                        }
                    } else {
                        self.gen_expr(arr, Self::EVAL_BASE);
                    }
                }
                self.push(0);
                self.gen_expr(idx, 1);
                self.pop(0);
                self.emit_index_scale(es, arr, elem_ty);
                self.emit_insn(&Insn::alu_rm_reg_reg(
                    crate::codegen::x86_64::opcode::AluOp::Add,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    1,
                )); // add rax, rcx
                if dst != 0 {
                    self.mov(dst, 0);
                }
            } else if matches!(e.as_ref(), Expr::CompoundLit { .. }) {
                // &CompoundLit: 复合字面量的值已经是地址, 直接使用
                self.gen_expr(e, dst);
            } else if let Expr::Real { e: inner, .. } = e.as_ref() {
                // &(__real expr): 实部地址 = 复数变量地址 (实部在偏移 0)
                if let Expr::Ident { name, .. } = inner.as_ref() {
                    if self.is_global_not_local(&name.name) {
                        self.emit_global_addr(dst, &name.name);
                    } else {
                        let off = self.stack_home_off(&name.name);
                        self.emit_lea_r13(dst, off);
                    }
                } else if !self.gen_lvalue_addr(inner, dst) {
                    self.gen_expr(inner, dst);
                }
                // 实部在偏移 0, 无需加偏移
            } else if let Expr::Imag { e: inner, .. } = e.as_ref() {
                // &(__imag expr): 虚部地址 = 复数变量地址 + inner_size
                let inner_ty = inner.ty();
                let inner_sz = match &inner_ty {
                    Type::Complex(ref c) => c.base.size() as i64,
                    _ => inner_ty.size() as i64,
                };
                if let Expr::Ident { name, .. } = inner.as_ref() {
                    if self.is_global_not_local(&name.name) {
                        self.emit_global_addr(dst, &name.name);
                    } else {
                        let off = self.stack_home_off(&name.name);
                        self.emit_lea_r13(dst, off);
                    }
                } else if !self.gen_lvalue_addr(inner, dst) {
                    self.gen_expr(inner, dst);
                }
                // 加上虚部偏移
                if inner_sz != 0 {
                    // add dst, inner_sz: 走 alu_rm_reg_imm32_b64 (恒 0x81 id)
                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, inner_sz));
                }
            }
        }
    }

    /// gen_expr `&&label` 标签地址分支 (batch F10 逐字搬迁: Expr::LabelAddr arm)。
    pub fn gen_expr_label_addr(&mut self, e: &Expr, dst: u8) {
        if let Expr::LabelAddr { label, .. } = e {
            // &&label: 加载标签地址到 dst (GNU 扩展)
            // 使用 LEA dst, [RIP + disp32] + label_addr_refs 延迟解析
            // 前缀标签名以避免与函数符号名冲突
            let label_name = self.c_label_to_asm(&label.name);
            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
            let modrm = 0x05 | ((dst & 7) << 3);
            self.emit_insn(&Insn::raw_bytes(&[rex, 0x8D, modrm, 0, 0, 0, 0]));
            let disp32_pos = self.pos() - 4;
            self.label_addr_refs.push((disp32_pos, label_name));
        }
    }
}
