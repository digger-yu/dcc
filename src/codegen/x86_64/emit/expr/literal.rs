// src/codegen/x86_64/emit/expr/literal.rs
// 字面量表达式 codegen (gen_expr 中的 Expr::Int / Char / Float / String arm)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4a): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_literal`, 语义 diff 为 0。旧影子 `gen_literal_expr` 自由函数
// 与 `mask_imm_to_type` / `make_string_label` 影子实现一并作废。

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::Codegen;

impl Codegen {
    /// gen_expr 中字面量分支的统一入口: 由调用方 (gen_expr) 按 `Expr::Int /
    /// Expr::Char / Expr::Float / Expr::String` 分派进来。
    pub fn gen_expr_literal(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Int { v, ty, .. } => {
                // 按类型宽度截断/零扩展常量值, 避免 mov_imm 对无符号小类型
                // 做错误的符号扩展 (例如 ~0U 被常量折叠为 -1, 应加载为
                // 0x00000000FFFFFFFF 而非 0xFFFFFFFFFFFFFFFF).
                let masked = Self::mask_imm_to_type(*v, ty);
                self.mov_imm(dst, masked);
            },
            Expr::Char { v, ty, .. } => {
                let masked = Self::mask_imm_to_type(*v, ty);
                self.mov_imm(dst, masked);
            },
            Expr::Float { c, ty, .. } => {
                // 浮点字面量: 存入 .rodata, 用 movsd/movss 加载到 xmm0
                // （long double 走 x87 fld tbyte，保留真 80-bit 精度）
                self.gen_float_imm_load(*c, ty);
            },
            Expr::String { data, .. } => {
                // 将字符串加入 .rodata 池, 生成 RIP-relative LEA
                let data = data.clone();
                if !self.string_pool.contains_key(&data) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), o);
                }
                let at = self.pos();
                // LEA dst, [RIP + disp32]  (7 bytes, disp32 = placeholder 0)
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]);
                self.string_refs.push((at, data));
            },
            _ => {
                // gen_expr 只在上述四种字面量 arm 调用本方法, 其他 arm 不可达。
                let _ = (dst, std::mem::discriminant(&Type::Int));
            },
        }
    }
    /// gen_expr sizeof 分支统一入口 (batch F1 逐字搬迁: Expr::SizeofTy / Expr::SizeofExpr arm)。
    pub fn gen_expr_sizeof(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::SizeofTy { ty, .. } => {
                // VM struct/union (含 VLA 字段的局部类型): sizeof 读
                // 声明时绑定槽, 不用当前变量值重算 (C11 6.2.4p7;
                // 20041218-2: 声明后 n++ 不影响 sizeof(struct s))
                let vm_key = match ty {
                    Type::Struct(n, _, _, false) => Some(format!("struct:{}", n)),
                    Type::Union(n, _, _, false) => Some(format!("union:{}", n)),
                    _ => None,
                };
                if let Some(k) = vm_key {
                    if let Some(&slot) = self.vm_struct_size_slots.get(&k) {
                        self.emit_mov_mem_r13(dst, slot);
                        return;
                    }
                }
                if Self::type_has_vla(ty) {
                    self.gen_sizeof_runtime(ty, dst);
                } else {
                    self.mov_imm(dst, ty.size() as i128);
                }
            },
            Expr::SizeofExpr { e, .. } => match e.as_ref() {
                // 字符串字面量: C 中为 char[N] 数组 (含 NUL), 返回实际字节数
                Expr::String { data, .. } => self.mov_imm(dst, data.len() as i128),
                _ => {
                    // VLA 变量的声明时绑定大小: 直接读分配槽 (基址变量
                    // 的总大小 / 2D 的行大小), 不重新求值维度表达式 —
                    // 后者会重复副作用 (pr22061-3: ++N 再自增)
                    if let Some(slot) = self.vla_bound_size_slot(e) {
                        self.emit_mov_mem_r13(dst, slot);
                    } else {
                        let ty = e.ty();
                        if Self::type_has_vla(&ty) {
                            self.gen_sizeof_runtime(&ty, dst);
                        } else {
                            self.mov_imm(dst, ty.size() as i128);
                        }
                    }
                },
            },
            _ => {},
        }
    }
    /// gen_expr alignof/offsetof 分支统一入口 (batch F2 逐字搬迁: Expr::AlignofTy /
    /// Expr::AlignofExpr / Expr::Offsetof arm)。结果均为常量, 直接 mov_imm 到 dst。
    pub fn gen_expr_alignof_offsetof(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::AlignofTy { ty, .. } => self.mov_imm(dst, ty.align() as i128),
            Expr::AlignofExpr { e, .. } => {
                // Special handling for function identifiers: use FuncDecl alignment
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&align) = self.func_alignments.get(&name.name) {
                        self.mov_imm(dst, align as i128);
                    } else {
                        // Not found or not a function - use type's default alignment
                        self.mov_imm(dst, e.ty().align() as i128);
                    }
                } else {
                    self.mov_imm(dst, e.ty().align() as i128);
                }
            },
            Expr::Offsetof {
                ty,
                ref member,
                ref indices,
                ..
            } => {
                let base_off = Self::compute_offset(ty, member);
                if indices.is_empty() {
                    self.mov_imm(dst, base_off as i128);
                } else {
                    // 找到字段的类型
                    let field_ty = Self::find_offsetof_field_type(ty, member);
                    // dst = base_off
                    self.mov_imm(dst, base_off as i128);
                    // 对每个下标: dst += idx * stride
                    let mut cur_ty = field_ty;
                    for idx_expr in indices.iter() {
                        if let Type::Array(elem, _) = &cur_ty {
                            // 计算下标值到 rcx
                            self.gen_expr(idx_expr, 1); // rcx = idx
                                                        // 有符号 32 位或更小整数需先符号扩展, 否则负索引乘 stride 错误
                            let idx_ty = idx_expr.ty();
                            if Self::is_signed_int(&idx_ty) && idx_ty.size() <= 4 {
                                // movslq %ecx, %rcx: 48 63 C9
                                self.emit(&[0x48, 0x63, 0xC9]);
                            }
                            // 计算 stride = sizeof(elem) 并乘到 rcx
                            if Self::type_has_vla(&elem.base) {
                                // VLA: 运行时计算 stride 到 r11
                                self.gen_sizeof_runtime(&elem.base, 11); // r11 = stride
                                                                         // rcx *= r11
                                self.imul_reg(1, 11);
                            } else {
                                // 常量 stride
                                let stride = elem.size() as u128;
                                if stride > 1 {
                                    if stride.is_power_of_two() {
                                        let shift = (stride.trailing_zeros() as u8).min(31);
                                        self.emit(&[0x48, 0xC1, 0xE1, shift]); // shl rcx, shift
                                    } else {
                                        self.imul_imm(1, stride as i128); // imul rcx, rcx, stride
                                    }
                                }
                            }
                            // dst += rcx
                            // add r/m64=dst(B), reg=rcx(R): dst>=8 需 REX.B(0x01)
                            let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                            self.emit(&[rex, 0x01, 0xC0 | ((1 & 7) << 3) | (dst & 7)]);
                            // 下降到元素类型
                            cur_ty = elem.base.clone();
                        }
                    }
                }
            },
            _ => {},
        }
    }
}
