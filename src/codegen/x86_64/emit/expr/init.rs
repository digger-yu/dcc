// src/codegen/x86_64/emit/expr/init.rs
// Expr::Assign arm 的 codegen (gen_expr 中的赋值 / 复合赋值分支, 含
// 聚合赋值 / 复数赋值 / 位域赋值 / Vector 赋值等)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4e): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_assign`, 语义 diff 为 0。旧影子 `gen_init_expr` /
// `handle_assignment` / `gen_compound_literal` / `gen_sizeof` 自由函数一并作废。

use crate::ast::{find_field, ArraySize, Expr, Type};
use crate::codegen::x86_64::Codegen;

impl Codegen {
    /// gen_expr 中 Expr::Assign arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_assign(&mut self, l: &Expr, r: &Expr, dst: u8) {
        // 检测 compound assignment 模式: Assign { l: target, r: Binop { l: read, r: rhs } }
        // 其中 target 和 read 结构相同 (由 parser desugaring 产生).
        // 如果 lvalue 含副作用 (如 *--ptr += 1), 必须只求值一次.
        // 策略: 先求值 lvalue 地址并保存, 然后从保存的地址读取, 计算 binop, 存回.
        if let Expr::Binop {
            l: ref binop_l,
            op: binop_op,
            r: ref binop_r,
            ..
        } = *r
        {
            // 检测 compound pattern: target 和 binop 的某个操作数结构相同
            // 注意: parser 对可交换运算符会交换操作数 (RHS 在左, read 在右)
            let compound: Option<&Expr> = if Self::exprs_structurally_equal(l, binop_l) {
                Some(binop_r.as_ref())
            } else if Self::exprs_structurally_equal(l, binop_r) {
                Some(binop_l.as_ref())
            } else {
                None
            };
            if let Some(compound_rhs) = compound {
                // 仅当 lvalue 含副作用时使用单次求值路径,
                // 否则走原有路径 (原有路径对所有类型都正确).
                if Self::expr_has_side_effects(l) {
                    self.gen_compound_assign_single_eval(l, compound_rhs, binop_op, dst);
                    return;
                }
            }
        }
        // sanitizer 插桩可能把 LHS 包成 Comma(check, lvalue):
        // 先执行 check 调用, 再对真实 lvalue 走 store 路径, 否则
        // 形态匹配失败会导致 store 静默丢失 (KASAN 下 *p = v 空操作)
        let mut lv: &Expr = l;
        while let Expr::Comma {
            l: check, r: inner, ..
        } = lv
        {
            self.gen_expr(check, 0);
            lv = inner.as_ref();
        }
        // 16 字节 vector 赋值到 Member/Index/Deref lvalue:
        // RHS 物化到向量临时槽 0, 再整块拷贝到 LHS 地址
        // (所有向量尺寸都走此路径: 8B 及更小向量若落入标量
        // store, rax 只是 gen_vector_load 最后加载元素的残值,
        // 会把单个元素位模式覆盖整个向量槽,
        // pr23135: res.v = i 后 i2[0] 变 i2[1] 的值)
        let assign_l_ty = lv.ty();
        if matches!(assign_l_ty, Type::Vector(_, _)) && !matches!(lv, Expr::Ident { .. }) {
            // 交付约定轴: va_arg 聚合的 rvalue 不物化到「向量临时槽 0」, 而是
            // rax = 缓冲区地址 (`gen_expr_va_arg` 写 `alloc_sret_slot` 的槽,
            // 与向量临时槽基址不是一个地方)。无条件按向量临时槽读源会把
            // 装载器填好的值用错误源整体覆盖, 故先按约定把真源压栈,
            // 取完 LHS 地址 (会覆写 rax) 再弹出作 rsi。
            let rax_is_agg_addr = match r {
                Expr::VaArg { ty, .. } => Self::va_arg_result_in_memory(ty),
                _ => false,
            };
            self.gen_expr(r, 0); // RHS → 向量临时槽 0 (或 rax = 聚合缓冲区地址)
            if rax_is_agg_addr {
                self.push(0);
            }
            if self.gen_lvalue_addr(lv, 0) {
                self.emit(&[0x48, 0x89, 0xC7]); // mov rdi, rax
                if rax_is_agg_addr {
                    self.pop(6); // rsi = 聚合缓冲区地址
                } else {
                    let toff = self.vector_temp_base(0, assign_l_ty.size());
                    self.emit_lea_r13(6, toff); // rsi = 临时槽
                }
                self.mov_imm(1, assign_l_ty.size() as i128);
                self.emit(&[0xFC]); // cld
                self.emit(&[0xF3, 0xA4]); // rep movsb
                return;
            }
            if rax_is_agg_addr {
                self.pop(6); // LHS 地址不可得, 落回下方通用路径: 保持栈平衡
            }
        }
        // _Complex lvalue 赋值 (非 Ident): 统一走物化+分量拷贝。
        // 下方 Index/Deref/Member 的标量 store 会把 rax 恒 8 字节
        // 直接写入: 标量 RHS 污染虚部 (a[i] = -1 得 (-1,-1) 而非
        // (-1,0), pr56837), complex RHS 则把 result_off 物化地址
        // 当数据写入。gen_lvalue_addr 失败时落回旧分支。
        if matches!(assign_l_ty, Type::Complex(_))
            && !matches!(lv, Expr::Ident { .. })
            && self.gen_lvalue_addr(lv, 0)
        {
            self.push(0); // 保存目标地址 (gen_expr 可能占用 rcx)
            self.gen_complex_store_rcx(r, &assign_l_ty);
            return;
        }
        if let Expr::Ident { name, ty: l_ty, .. } = lv {
            let sz = l_ty.size();
            // VM struct/union (含 VLA 字段的局部类型) 即使编译期
            // size() 回退值 <= 8 也必须走整体拷贝: 其语义大小是
            // 运行时值, 标量 8 字节路径会把聚合 rvalue (地址约定)
            // 当内容存入 (20070919-1: b = p[2] 后 b.w 变地址字节)
            let is_aggregate =
                matches!(
                    l_ty,
                    Type::Struct(_, _, _, _)
                        | Type::Union(_, _, _, _)
                        | Type::Array(_, ArraySize::Const(_))
                        | Type::Atomic(_)
                        | Type::Vector(_, _)
                ) && (sz > 8 || matches!(r, Expr::CompoundLit { .. }) || l_ty.has_vla());
            if is_aggregate {
                // struct/array 整体赋值: 逐块拷贝
                self.gen_aggregate_assign(&name.name, l_ty, r);
                // 赋值表达式的值 = 左值转换后的值 (C11 6.5.16p3)。
                // 聚合 rvalue 统一约定为地址: 供链式聚合赋值
                // (a = (b = c)) 与语句表达式块值 (20070919-1:
                // a = ({ ...; p[3] = b; })) 继续传递。
                if self.is_global_not_local(&name.name) {
                    self.emit_global_addr(dst, &name.name);
                } else if let Some(&off) = self.vla_offsets.get(&name.name) {
                    // VM struct/VLA 变量: 地址 = 数据区基址
                    self.load_stack(dst, off);
                } else {
                    let aoff = self.var_offset(&name.name);
                    self.emit_lea_r13(dst, aoff);
                }
            } else if matches!(l_ty, Type::Complex(_)) {
                // _Complex 赋值: 统一走整体拷贝。8B 的 rax 单槽路径
                // 无法表达浮点 complex rvalue (值在 xmm0, rax 是地址/垃圾)
                self.gen_aggregate_assign(&name.name, l_ty, r);
            } else {
                self.gen_expr_decay(r, dst); // 数组衰减: int *p = arr
                                             // Implicit float→int conversion when RHS is
                                             // float but LHS is integer (e.g. `a = 0.0;`).
                let r_ty = r.ty();
                if matches!(&r_ty, Type::Float | Type::Double | Type::LongDouble)
                    && Self::is_integer_type(l_ty)
                {
                    self.emit_implicit_float_to_int(dst, &r_ty, l_ty);
                }
                // Implicit float→float precision conversion
                // when both sides are float but different
                // precision (e.g. `float f = double_expr;`).
                // Without cvtsd2ss, movss would store the raw
                // lower 32 bits of the double, not the float
                // representation.
                if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble)
                    && matches!(&r_ty, Type::Float | Type::Double | Type::LongDouble)
                {
                    self.emit_float_precision_convert(&r_ty, l_ty);
                }
                // Implicit int→float conversion when RHS is
                // integer but LHS is float (e.g. `ping = 0;`).
                // The integer result sits in eax; convert it to
                // xmm0 before gen_float_store.
                if matches!(l_ty, Type::Float | Type::Double) && Self::is_integer_type(&r_ty) {
                    self.emit_scalar_to_xmm0_if_needed(r, l_ty);
                }
                // 赋值表达式的值是左值转换后的值 (C11 6.5.16p3),
                // 而非右值原值: `ul = us = -1` 中内层 us=-1 的结果须
                // 先截断到 unsigned short (65535) 再传播给外层, 否则
                // dst 中未截断的 64 位 -1 直接作为内层表达式的值传出,
                // ul 得到 0xFFFFFFFFFFFFFFFF.
                // 仅窄于 4 字节的整数左值需要重扩展: 4/8 字节槽由
                // mov_from_stack/emit_global_store 按声明宽度读取,
                // 低位已含正确值; 浮点右值在 xmm0 不经 dst.
                let r_ty = r.ty();
                let l_sz = l_ty.size();
                if l_sz < 4 && (Self::is_integer_type(&r_ty) || matches!(r_ty, Type::Enum(_, _, _)))
                {
                    // 重扩展方向由左值类型符号性决定 (char 按有符号,
                    // 固定底层类型枚举按底层类型), 取 dst 低 8/16 位
                    // 重扩展到 64 位: 有符号 movsx, 无符号 movzx
                    let l_signed = match l_ty {
                        Type::Enum(_, _, Some(u)) => Self::is_signed_int(&u.base),
                        Type::Bool | Type::UChar | Type::UShort => false,
                        _ => true,
                    };
                    // reg 与 r/m 同为 dst: dst >= r8 时 REX 需同时置 R|B
                    let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                    let modrm = 0xC0 | ((dst & 7) << 3) | (dst & 7);
                    match (l_sz, l_signed) {
                        (1, true) => self.emit(&[rex, 0x0F, 0xBE, modrm]),
                        (1, false) => self.emit(&[rex, 0x0F, 0xB6, modrm]),
                        (2, true) => self.emit(&[rex, 0x0F, 0xBF, modrm]),
                        (2, false) => self.emit(&[rex, 0x0F, 0xB7, modrm]),
                        _ => {},
                    }
                }
                if matches!(l_ty, Type::Int128 | Type::UInt128) {
                    // RHS 非 __int128 时, rdx 未设置, 需零扩展/符号扩展
                    let r_ty_for_128 = r.ty();
                    if !matches!(r_ty_for_128, Type::Int128 | Type::UInt128) {
                        if matches!(l_ty, Type::UInt128)
                            || matches!(
                                r_ty_for_128,
                                Type::Bool
                                    | Type::UChar
                                    | Type::UShort
                                    | Type::UInt
                                    | Type::ULong
                                    | Type::ULongLong
                            )
                        {
                            self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
                        } else {
                            self.emit(&[0x48, 0x99]); // cqo (sign-extend rax→rdx:rax)
                        }
                    }
                    if self.is_global_not_local(&name.name) {
                        self.gen_int128_store(&name.name);
                    } else if self.current_func_is_nested
                        && self.upvar_offsets.contains_key(&name.name)
                    {
                        let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                        self.store_upvar(0, parent_off, 8, &name.name);
                        self.store_upvar(2, parent_off + 8, 8, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        self.store_stack(0, off);
                        self.store_stack(2, off + 8);
                    }
                } else if self.is_global_not_local(&name.name) {
                    if matches!(l_ty, Type::Vector(_, _)) {
                        self.gen_vector_store(&name.name);
                    } else if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble)
                        || matches!(l_ty, Type::Imaginary(_))
                    {
                        // 浮点全局: 值在通道 (xmm0 / st0), 按目标宽度存储
                        self.emit_global_float_store(&name.name, l_ty, &r_ty);
                    } else {
                        self.emit_global_store(dst, &name.name);
                    }
                } else if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name)
                {
                    let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                    self.store_upvar(dst, parent_off, l_ty.size(), &name.name);
                } else if matches!(l_ty, Type::Vector(_, _)) {
                    self.gen_vector_store(&name.name);
                } else if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble)
                    || matches!(l_ty, Type::Imaginary(_))
                {
                    self.gen_float_store(&name.name, l_ty);
                } else {
                    self.mov_to_stack(dst, &name.name);
                }
            }
        } else if let Expr::Index {
            arr,
            idx,
            ty: elem_ty,
            ..
        } = lv
        {
            // arr[i] = val
            // 先尝试语义层路径计算地址
            use crate::semantics::{compute_lvalue_address, AddressContext, MaterializeAction};
            let ctx = AddressContext {
                is_nested: self.current_func_is_nested,
            };

            // 描述符自描述多维下标链 (`ByteOffset::Runtime.terms` 逐项携带
            // 自己的下标表达式), 旧的「嵌套运行时索引」浅层试探已删除:
            // 无法表达的形状由 materialize_address 返回 false, 落到下面旧路径。
            let desc = Some(compute_lvalue_address(self, lv, &ctx));

            let es = elem_ty.size() as i32;

            // 聚合元素赋值 (VM struct / >8B struct): rep movsb
            // 整体拷贝。标量 store 会把聚合 rvalue (地址约定)
            // 或 8 字节内容截断写入, 语义都错 (20070919-1)
            let is_agg_elem = matches!(
                elem_ty,
                Type::Struct(_, _, _, _)
                    | Type::Union(_, _, _, _)
                    | Type::Array(_, ArraySize::Const(_))
                    | Type::Atomic(_)
            ) && (es > 8 || elem_ty.has_vla());

            if let Some(ref desc) = desc {
                if is_agg_elem {
                    // 聚合元素: 用语义层计算地址到 rax, 然后 push + gen_agg_elem_store
                    if self.materialize_address(
                        desc,
                        MaterializeAction::TakeAddress,
                        Some(idx),
                        lv,
                        Some(arr),
                        0,
                    ) {
                        self.push(0); // save addr
                        self.gen_agg_elem_store(r, elem_ty, es, dst);
                        return;
                    }
                } else if matches!(elem_ty, Type::Complex(ref c)
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble))
                {
                    // _Complex 浮点元素: 用语义层计算地址到 rax, 然后 store_complex_at_rcx
                    if self.materialize_address(
                        desc,
                        MaterializeAction::TakeAddress,
                        Some(idx),
                        lv,
                        Some(arr),
                        0,
                    ) {
                        self.push(0); // save addr
                        if let Type::Complex(ref cinner) = elem_ty {
                            // store_complex_at_rcx 自行 pop 地址并对 r 单次求值
                            self.store_complex_at_rcx(r, &cinner.base);
                        }
                        return;
                    }
                } else if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                    // 浮点元素: 用语义层计算地址, 然后存储
                    if self.materialize_address(
                        desc,
                        MaterializeAction::TakeAddress,
                        Some(idx),
                        lv,
                        Some(arr),
                        0,
                    ) {
                        self.push(0); // save addr
                        self.gen_expr(r, 0); // rax = val
                        self.pop(1); // rcx = addr
                        self.emit_seg(self.lvalue_seg_prefix(lv));
                        self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1); // 单一真源：收浮影子副本
                        return;
                    }
                } else {
                    // 标量元素: 用语义层计算地址, 然后存储
                    if self.materialize_address(
                        desc,
                        MaterializeAction::TakeAddress,
                        Some(idx),
                        lv,
                        Some(arr),
                        0,
                    ) {
                        self.push(0); // save addr
                        self.gen_expr(r, 0); // rax = val
                        self.pop(1); // rcx = addr
                        self.emit_seg(self.lvalue_seg_prefix(lv));
                        match es {
                            1 => self.emit(&[0x88, 0x01]),
                            2 => self.emit(&[0x66, 0x89, 0x01]),
                            4 => self.emit(&[0x89, 0x01]),
                            _ => self.emit(&[0x48, 0x89, 0x01]),
                        }
                        return;
                    }
                }
            }

            // Fallback: 旧路径 (语义层无法处理时使用)
            if let Expr::Ident {
                name: arr_name,
                ty: arr_ty,
                ..
            } = arr.as_ref()
            {
                // 统一基址计算 (修复 BUG-C: 使用 resolve_global_key 解析 static local)
                self.emit_ident_base_addr(&arr_name.name, arr_ty, 0);
                self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                self.gen_expr(idx, 1); // rcx = index
                self.pop(0); // 恢复基址到 rax
                self.emit_index_scale(es, arr, elem_ty);
                self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx (addr in rax)
                self.push(0); // save addr
                let is_cx_float = matches!(elem_ty, Type::Complex(ref c)
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble));
                if is_cx_float {
                    // store_complex_at_rcx 自行 pop 地址并对 r 单次求值,
                    // 不在此预 gen_expr（副作用 rvalue 会被求值两次）。
                    if let Type::Complex(ref cinner) = elem_ty {
                        self.store_complex_at_rcx(r, &cinner.base);
                    }
                } else {
                    self.gen_expr(r, 0); // rax = val
                    self.pop(1); // rcx = addr
                    if is_agg_elem {
                        self.gen_agg_elem_store(r, elem_ty, es, dst);
                        return;
                    }
                    if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                        self.emit_seg(self.lvalue_seg_prefix(lv));
                        self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1);
                    } else {
                        self.emit_seg(self.lvalue_seg_prefix(lv));
                        match es {
                            1 => self.emit(&[0x88, 0x01]),
                            2 => self.emit(&[0x66, 0x89, 0x01]),
                            4 => self.emit(&[0x89, 0x01]),
                            _ => self.emit(&[0x48, 0x89, 0x01]),
                        }
                    }
                }
            } else {
                // 通用路径: 基址来自任意 lvalue/指针表达式 (如 s.i[k] = v)
                let arr_ty = arr.ty();
                if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                    if !self.gen_lvalue_addr(arr, 0) {
                        self.gen_expr(arr, 0);
                    }
                } else {
                    self.gen_expr(arr, 0);
                }
                self.push(0);
                self.gen_expr(idx, 1);
                self.pop(0);
                self.emit_index_scale(es, arr, elem_ty);
                self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx (addr in rax)
                self.push(0);
                let is_cx_float2 = matches!(elem_ty, Type::Complex(ref c)
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble));
                if is_cx_float2 {
                    if let Type::Complex(ref cinner) = elem_ty {
                        self.store_complex_at_rcx(r, &cinner.base);
                    }
                } else {
                    self.gen_expr(r, 0);
                    self.pop(1);
                    if is_agg_elem {
                        self.gen_agg_elem_store(r, elem_ty, es, dst);
                        return;
                    }
                    if matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
                        self.emit_seg(self.lvalue_seg_prefix(lv));
                        self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1);
                    } else {
                        self.emit_seg(self.lvalue_seg_prefix(lv));
                        match es {
                            1 => self.emit(&[0x88, 0x01]),
                            2 => self.emit(&[0x66, 0x89, 0x01]),
                            4 => self.emit(&[0x89, 0x01]),
                            _ => self.emit(&[0x48, 0x89, 0x01]),
                        }
                    }
                }
            }
        } else if let Expr::Deref { e: ptr_e, .. } = lv {
            // *ptr = val
            // 前缀沿基址链下潜取（单一入口 `lvalue_seg_prefix`）：只看本层会把
            // `*pp` 这类形状漏掉。
            let seg_prefix = self.lvalue_seg_prefix(lv);
            let is_deref_aggregate = matches!(
                assign_l_ty,
                Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _)
            ) && (assign_l_ty.size() > 8 || assign_l_ty.has_vla());
            if is_deref_aggregate {
                // Aggregate *ptr assignment: use rep movsb
                // to correctly handle packed/misaligned structs.
                let agg_sz = assign_l_ty.size();
                // 1. Get source address into rsi
                let rhs_is_lvalue =
                    matches!(r, Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. });
                let rhs_is_ident = matches!(r, Expr::Ident { .. });
                let rhs_is_compound = matches!(r, Expr::CompoundLit { .. });
                let rhs_is_call = matches!(r, Expr::Call { ty: ref cty, .. }
                            if Self::needs_memory_return(cty));
                if rhs_is_compound {
                    // CompoundLit: gen_expr builds on stack,
                    // returns address in rax
                    self.gen_expr(r, 0);
                    self.mov(6, 0); // rsi = rax
                } else if rhs_is_call {
                    // Call returning aggregate: gen_expr leaves
                    // rax = &sret buffer (or &temp for reg return).
                    // Use rax directly as source address.
                    self.gen_expr(r, 0);
                    self.mov(6, 0); // rsi = rax (source addr)
                } else if rhs_is_ident {
                    if let Expr::Ident { name, .. } = r {
                        if self.is_global_not_local(&name.name) {
                            self.emit_global_addr(6, &name.name);
                        } else {
                            let src_off = self.var_offset(&name.name);
                            self.emit_lea_r13(6, src_off);
                        }
                    }
                } else if rhs_is_lvalue {
                    self.gen_lvalue_addr(r, 0); // rax = src addr
                    self.mov(6, 0); // rsi = rax
                } else {
                    // General: gen_expr, store to temp
                    self.gen_expr(r, 0);
                    let temp_off = self.var_offset("<agg_deref_tmp>");
                    self.store_stack(0, temp_off);
                    if agg_sz > 8 {
                        self.store_stack(2, temp_off + 8);
                    }
                    self.emit_lea_r13(6, temp_off);
                }
                // 2. Get destination address (pointer value) into rdi
                self.gen_expr(ptr_e, 0); // rax = ptr value
                self.mov(7, 0); // rdi = rax
                                // 3. 拷贝到段限定目标（带段时不能用 rep movsb，
                                // 理由与站点说明见 `Codegen::copy_agg_to_seg`）
                self.emit(&[0xFC]); // cld
                                    // 拷贝大小: VM struct/union 为运行时值 (声明点绑定
                                    // 槽优先), 其余为编译期 size
                if assign_l_ty.has_vla() {
                    if let Some(slot) = self.vm_size_slot(&assign_l_ty) {
                        self.emit_mov_mem_r13(1, slot);
                    } else {
                        self.gen_sizeof_runtime(&assign_l_ty, 11);
                        self.mov(1, 11);
                    }
                } else {
                    self.mov_imm(1, agg_sz as i128); // rcx = size
                }
                match seg_prefix {
                    None => self.emit(&[0xF3, 0xA4]), // rep movsb
                    Some(seg) if assign_l_ty.has_vla() => self.copy_bytes_to_seg_runtime(seg),
                    Some(seg) => self.copy_agg_to_seg(agg_sz, seg),
                }
            } else {
                self.gen_expr(ptr_e, 0); // rax = address
                self.push(0); // save addr
                let is_cx_ptr = matches!(assign_l_ty, Type::Complex(ref c)
                        if matches!(&c.base,
                            Type::Float | Type::Double | Type::LongDouble));
                if is_cx_ptr {
                    if let Some(seg) = seg_prefix {
                        self.emit(&[seg]);
                    }
                    if let Type::Complex(ref cinner) = assign_l_ty {
                        self.store_complex_at_rcx(r, &cinner.base);
                    }
                } else {
                    self.gen_expr(r, 0); // rax = val
                    self.pop(1); // rcx = addr
                                 // __seg_gs/__seg_fs 指针存储: 发射段前缀 (%gs:/%fs:)
                    if let Some(seg) = seg_prefix {
                        self.emit(&[seg]);
                    }
                    if matches!(&assign_l_ty, Type::Float | Type::Double | Type::LongDouble) {
                        self.emit_store_scalar_float_at(&r.ty(), &assign_l_ty, 1);
                    } else {
                        // 按 *ptr 类型宽度选择 store 指令 (截断语义:
                        // 只写低 n 字节). 旧实现恒用 8 字节 movq,
                        // 会越界覆盖相邻内存 (如 decNumber
                        // *up=(Unit)out 破坏后续单元 → 系数全 0).
                        match assign_l_ty.size() {
                            1 => self.emit(&[0x88, 0x01]),       // mov [rcx], al
                            2 => self.emit(&[0x66, 0x89, 0x01]), // mov [rcx], ax
                            4 => self.emit(&[0x89, 0x01]),       // mov [rcx], eax
                            _ => self.emit(&[0x48, 0x89, 0x01]), // mov [rcx], rax
                        }
                    }
                }
                // pop(1) 把 rcx 覆盖成目标地址, store 后结果值仍在 rax.
                // 调用方若要求 dst=rcx (如 shift amount), 这里必须
                // 重新把 rax 搬到 rcx, 否则 shift 操作会用错误的
                // 移位计数 (之前是目标地址的低字节).
                if dst == 1 {
                    self.mov(1, 0); // mov rcx, rax
                }
            }
        } else if let Expr::Member {
            expr,
            field,
            is_arrow,
            ..
        } = lv
        {
            // struct.field = val 或 ptr->field = val
            // 1. 查找字段信息 (支持匿名 struct/union)
            //    注意: `->` 时 expr.ty() 是指针类型, 须解引用后再查字段
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
            let is_bitfield = field_info.as_ref().is_some_and(|f| f.bit_width.is_some());
            // 2. 计算目标地址
            if *is_arrow {
                self.gen_expr(expr, 0); // rax = 指针值
            } else if let Expr::Ident { name, .. } = expr.as_ref() {
                if self.is_global_not_local(&name.name) {
                    // 全局结构体: RIP-relative LEA
                    self.emit_global_addr(0, &name.name);
                } else {
                    let off = self.var_offset(&name.name);
                    // VLA/VM struct 变量: [off] 存数据基址, 须加载
                    // 而非 lea 槽地址 (否则寻址到元数据槽,
                    // pr82210: s.a[i].c 写坏基址槽)
                    if self.vla_offsets.contains_key(&name.name) {
                        self.load_stack(0, off);
                    } else {
                        // LEA rax, [r13 + off]
                        self.emit(&[0x49, 0x8D]);
                        if off.abs() < 128 {
                            self.emit(&[0x45, off as i8 as u8]); // mod=01, rm=r13
                        } else {
                            self.emit(&[0x85]); // mod=00, rm=r13
                            self.emit(&(off as i32).to_le_bytes());
                        }
                    }
                }
            } else if !self.gen_lvalue_addr(expr, 0) {
                // 通用路径失败: 退回直接求值 (旧行为)
                self.gen_expr(expr, 0);
            }
            // 3. 加上字段偏移
            if let Some(ref f) = field_info {
                let field_off = f.offset as i32;
                if field_off != 0 {
                    self.emit(&[0x48, 0x05]);
                    self.emit(&field_off.to_le_bytes());
                }
            }
            if is_bitfield {
                // 位域写入: 读-改-写 (使用帧内临时槽, 按嵌套深度动态分配)
                let f = field_info.as_ref().unwrap();
                let bw = f.bit_width.unwrap();
                let container_size = f.ty.size();
                let bit_off = f.bit_offset;
                // 按嵌套深度计算帧槽偏移 (避免嵌套位域 RMW 互相覆盖)
                let depth = self.bf_rmw_depth as i32;
                let addr_off = (-8 - depth * 16) as i8; // 地址槽
                let val_off = (-16 - depth * 16) as i8; // 值槽
                                                        // rax = 容器地址
                                                        // 保存地址到帧临时槽1: [r13 + addr_off]
                self.emit(&[0x49, 0x89, 0x45, addr_off as u8]); // mov [r13+addr_off], rax
                                                                // 加载当前容器值 (结果统一放入 rax)
                match container_size {
                    4 => self.emit(&[0x8B, 0x00]),       // mov eax, [rax] (零扩展)
                    8 => self.emit(&[0x48, 0x8B, 0x00]), // mov rax, [rax]
                    2 => {
                        self.emit(&[0x66, 0x8B, 0x00]); // mov ax, [rax]
                        self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
                    }, // movzx eax, ax
                    1 => self.emit(&[0x0F, 0xB6, 0x00]), // movzx eax, byte [rax]
                    _ => {},
                }
                // 保存容器值到帧临时槽2: [r13 + val_off]
                self.emit(&[0x49, 0x89, 0x45, val_off as u8]); // mov [r13+val_off], rax
                                                               // 计算右值 (递增嵌套深度, 防止内部位域 RMW 覆盖本层帧槽)
                self.bf_rmw_depth += 1;
                self.gen_expr(r, 0); // rax = 新值
                self.bf_rmw_depth -= 1;
                // 掩码: and rax, (1<<bw)-1
                if bw < 64 {
                    let mask: u64 = (1u64 << bw) - 1;
                    if mask <= 0xFFFFFFFF {
                        self.emit(&[0x48, 0x25]); // and eax, imm32
                        self.emit(&(mask as u32).to_le_bytes());
                    } else {
                        // >32 位掩码: movabs rdx, mask; and rax, rdx
                        self.emit(&[0x48, 0xBA]); // movabs rdx, imm64
                        self.emit(&mask.to_le_bytes());
                        self.emit(&[0x48, 0x21, 0xD0]); // and rax, rdx
                    }
                }
                // 左移到位置: shl rax, bit_off
                if bit_off > 0 {
                    self.emit(&[0x48, 0xC1, 0xE0, bit_off as u8]); // shl rax, imm8
                }
                // rax = 移位后的新值
                // 重新从内存加载容器值 (RHS 求值可能已修改内存中的容器)
                // rcx = [r13 + addr_off] (地址)
                self.emit(&[0x49, 0x8B, 0x4D, addr_off as u8]); // mov rcx, [r13+addr_off]
                                                                // rcx = [rcx] (从内存重新加载容器)
                match container_size {
                    4 => self.emit(&[0x8B, 0x09]),       // mov ecx, [rcx]
                    8 => self.emit(&[0x48, 0x8B, 0x09]), // mov rcx, [rcx]
                    2 => {
                        self.emit(&[0x66, 0x8B, 0x09]);
                        self.emit(&[0x48, 0x0F, 0xB7, 0xC9]);
                    },
                    1 => self.emit(&[0x0F, 0xB6, 0x09]),
                    _ => {},
                }
                // 清除目标位: inv_mask = ~(((1<<bw)-1) << bit_off)
                let mask: u64 = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
                let inv_mask = !(mask << bit_off);
                // and rcx, inv_mask
                if (inv_mask as u32 as i32 as i64 as u64) == inv_mask {
                    // 可以用 imm32 符号扩展
                    self.emit(&[0x48, 0x81, 0xE1]); // and rcx, imm32
                    self.emit(&(inv_mask as u32).to_le_bytes());
                } else {
                    // >32 位: movabs rdx, inv_mask; and rcx, rdx
                    self.emit(&[0x48, 0xBA]); // movabs rdx, imm64
                    self.emit(&inv_mask.to_le_bytes());
                    self.emit(&[0x48, 0x21, 0xD1]); // and rcx, rdx
                }
                // or rcx, rax
                self.emit(&[0x48, 0x09, 0xC1]); // or rcx, rax
                                                // 加载地址: rdx = [r13 + addr_off]
                self.emit(&[0x49, 0x8B, 0x55, addr_off as u8]); // mov rdx, [r13+addr_off]
                                                                // 存储容器 (结果在 rcx 中)
                match container_size {
                    4 => self.emit(&[0x89, 0x0A]),       // mov [rdx], ecx
                    8 => self.emit(&[0x48, 0x89, 0x0A]), // mov [rdx], rcx
                    2 => self.emit(&[0x66, 0x89, 0x0A]), // mov [rdx], cx
                    1 => self.emit(&[0x88, 0x0A]),       // mov [rdx], cl
                    _ => {},
                }
                // 位域赋值表达式的结果 = 截断并符号扩展后的值 (C11 6.5.16p3)
                // 从内存重新加载容器，提取位域值并符号扩展
                {
                    let f = field_info.as_ref().unwrap();
                    let is_signed = Self::is_signed_int(&f.ty);
                    // rdx = [r13 + addr_off] (地址)
                    self.emit(&[0x49, 0x8B, 0x55, addr_off as u8]); // mov rdx, [r13+addr_off]
                                                                    // rdx = [rdx] (重新加载容器)
                    match container_size {
                        4 => self.emit(&[0x8B, 0x12]),       // mov edx, [rdx]
                        8 => self.emit(&[0x48, 0x8B, 0x12]), // mov rdx, [rdx]
                        2 => {
                            self.emit(&[0x66, 0x8B, 0x12]);
                            self.emit(&[0x48, 0x0F, 0xB7, 0xD2]);
                        },
                        1 => self.emit(&[0x0F, 0xB6, 0x12]),
                        _ => {},
                    }
                    // shr rdx, bit_off
                    if bit_off > 0 {
                        self.emit(&[0x48, 0xC1, 0xEA, bit_off as u8]); // shr rdx, imm8
                    }
                    // mask to bw bits
                    if bw < 64 {
                        let mask: u64 = (1u64 << bw) - 1;
                        if mask <= 0xFFFFFFFF {
                            self.emit(&[0x48, 0x81, 0xE2]); // and rdx, imm32
                            self.emit(&(mask as u32).to_le_bytes());
                        } else {
                            self.emit(&[0x48, 0xBA]); // movabs rdx, mask
                            self.emit(&mask.to_le_bytes());
                            // re-load address for the and
                            self.emit(&[0x49, 0x8B, 0x45, addr_off as u8]); // mov rax, [r13+addr_off]
                                                                            // Actually this path is complex; for bw<=63 mask fits in 32 bits
                                                                            // since (1<<63)-1 > u32::MAX but bw<64 means bw<=63
                                                                            // (1u64<<63)-1 = 0x7FFFFFFFFFFFFFFF which doesn't fit in u32
                                                                            // Fall through to movabs path
                            self.emit(&[0x48, 0x21, 0xC2]); // and rdx, rax
                        }
                    }
                    // sign-extend if signed bitfield
                    if is_signed && bw < 64 {
                        let shift = 64 - bw;
                        // shl rdx, shift
                        self.emit(&[0x48, 0xC1, 0xE2, shift as u8]); // shl rdx, imm8
                                                                     // sar rdx, shift
                        self.emit(&[0x48, 0xC1, 0xFA, shift as u8]); // sar rdx, imm8
                    }
                    // move result to dst
                    if dst != 2 {
                        self.mov(dst, 2); // dst = rdx
                    }
                }
            } else {
                // Check if field type is aggregate (struct/union/array)
                let field_is_aggregate = field_info.as_ref().is_some_and(|f| {
                    matches!(
                        &f.ty,
                        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _)
                    )
                });
                if field_is_aggregate {
                    // Aggregate member assignment: use rep movsb to copy full struct
                    let agg_sz = field_info.as_ref().map_or(0, |f| f.ty.size());
                    // Determine how to get the source address (rsi).
                    // For lvalue expressions (Member/Ident/Index/Deref),
                    // gen_expr does NOT materialize the struct value in
                    // rax:rdx (struct > 8B has no scalar rvalue). Instead
                    // we must use gen_lvalue_addr to get the address.
                    let rhs_is_lvalue =
                        matches!(r, Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. });
                    let rhs_is_ident = matches!(r, Expr::Ident { .. });
                    let rhs_is_compound = matches!(r, Expr::CompoundLit { .. });
                    self.push(0); // save target address
                    if rhs_is_lvalue {
                        // Lvalue RHS: get source address directly
                        self.gen_lvalue_addr(r, 0); // rax = source address
                        self.pop(1); // rcx = target address
                        self.emit(&[0x48, 0x89, 0xC6]); // mov rsi, rax
                    } else if rhs_is_ident {
                        // Ident RHS: struct variable address via LEA
                        if let Expr::Ident { name, .. } = r {
                            if self.is_global_not_local(&name.name) {
                                self.emit_global_addr(6, &name.name); // rsi = global addr
                            } else {
                                let src_off = self.var_offset(&name.name);
                                self.emit_lea_r13(6, src_off); // rsi = local addr
                            }
                        }
                        self.pop(1); // rcx = target address
                    } else if rhs_is_compound {
                        self.gen_expr(r, 0); // rax = compound lit temp address
                        self.pop(1); // rcx = target address
                        self.emit(&[0x48, 0x89, 0xC6]); // mov rsi, rax
                    } else {
                        // General RHS: evaluate, store rax:rdx to temp
                        self.gen_expr(r, 0);
                        self.pop(1); // rcx = target address
                        let temp_off = self.var_offset("<agg_member_tmp>");
                        self.store_stack(0, temp_off);
                        if agg_sz > 8 {
                            self.store_stack(2, temp_off + 8);
                        }
                        self.emit_lea_r13(6, temp_off); // rsi = source
                    }
                    self.emit(&[0x48, 0x89, 0xCF]); // mov rdi, rcx (dst)
                    self.emit(&[0xFC]); // cld
                    self.mov_imm(1, agg_sz as i128); // rcx = size (rep movsb counter)
                    self.emit(&[0xF3, 0xA4]); // rep movsb
                } else {
                    self.push(0); // 保存目标地址
                    let size = field_info.as_ref().map_or(4, |f| f.ty.size());
                    // 浮点字段: 值在 xmm0, 用 movss/movsd 存储
                    let fty = field_info.as_ref().map(|f| f.ty.clone());
                    let is_cx_field = matches!(&fty,
                        Some(Type::Complex(ref c))
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble));
                    if is_cx_field {
                        if let Some(Type::Complex(ref cinner)) = fty {
                            // 自行 pop 地址并对 r 单次求值
                            self.store_complex_at_rcx(r, &cinner.base);
                        }
                    } else {
                        self.gen_expr(r, 0); // rax = 值 (浮点: xmm0)
                        self.pop(1); // rcx = 目标地址
                        if let Some(fty) = &fty {
                            if let Type::Complex(ref cinner) = fty {
                                // 整数 complex 字段: rvalue 已由外层
                                // gen_expr 物化在 result_off (rax=地址
                                // 约定), 整体拷贝 isz*2 字节到 [rcx]
                                let isz = cinner.base.size();
                                let result_off = -self.frame_size + 8;
                                self.emit_lea_r13(6, result_off); // rsi
                                self.emit(&[0x48, 0x89, 0xCF]); // mov rdi, rcx
                                self.mov_imm(1, (isz * 2) as i128);
                                self.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
                            } else if matches!(fty, Type::Float | Type::Double | Type::LongDouble) {
                                self.emit_store_scalar_float_at(&r.ty(), fty, 1);
                            // 单一真源：收浮影子副本
                            } else {
                                match size {
                                    1 => self.emit(&[0x88, 0x01]),
                                    2 => self.emit(&[0x66, 0x89, 0x01]),
                                    4 => self.emit(&[0x89, 0x01]),
                                    8 => self.emit(&[0x48, 0x89, 0x01]),
                                    16 => {
                                        // __int128: rax:rdx → [rcx]:[rcx+8]
                                        self.emit(&[0x48, 0x89, 0x01]); // mov [rcx], rax
                                        self.emit(&[0x48, 0x89, 0x51, 0x08]); // mov [rcx+8], rdx
                                    },
                                    _ => {},
                                }
                            }
                        } else {
                            match size {
                                1 => self.emit(&[0x88, 0x01]),
                                2 => self.emit(&[0x66, 0x89, 0x01]),
                                4 => self.emit(&[0x89, 0x01]),
                                8 => self.emit(&[0x48, 0x89, 0x01]),
                                16 => {
                                    self.emit(&[0x48, 0x89, 0x01]); // mov [rcx], rax
                                    self.emit(&[0x48, 0x89, 0x51, 0x08]); // mov [rcx+8], rdx
                                },
                                _ => {},
                            }
                        }
                    }
                }
            }
        } else if let Expr::Real { e: ce, .. } | Expr::Imag { e: ce, .. } = lv {
            // __real__/__imag__ 赋值: 写 _Complex 变量的分量.
            // 分量偏移: 实部 0, 虚部 +inner_sz (LongDouble 为 -8-inner_sz)
            let is_real = matches!(lv, Expr::Real { .. });
            let inner = match ce.ty() {
                Type::Complex(i) => i.base.clone(),
                Type::Imaginary(i) => i.base.clone(),
                _ => Type::Double,
            };
            let is_imag_var = matches!(ce.ty(), Type::Imaginary(_));
            let inner_sz = inner.size() as i64;
            let is_float_inner = matches!(inner, Type::Float | Type::Double | Type::LongDouble);
            let part_off = if is_imag_var {
                // _Imaginary 变量只有一个分量 (虚部), 偏移 0
                0
            } else if is_real {
                0
            } else {
                inner_sz
            };
            if self.gen_lvalue_addr(ce, 0) {
                self.push(0); // 保存目标地址
                self.gen_expr(r, 0); // 值 (浮点: xmm0)
                self.pop(1); // rcx = 目标地址
                if matches!(inner, Type::LongDouble) {
                    // LongDouble 分量: gen_expr 后值在 st0 (80-bit tbyte),
                    // 必须 fstpt 直写 [rcx+part_off]。绝不能走下方 SSE
                    // movsd — 那只会写 8 字节且取的是 xmm0 (垃圾),
                    // 破坏 real/imag 的 10 字节 tbyte 槽。
                    self.emit_x87_fstp_at(1, part_off as i32, &inner);
                } else if is_float_inner {
                    self.emit_scalar_to_xmm0_if_needed(r, &inner);
                    let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                    self.emit(&[pfx, 0x0F, 0x11]); // movss/movsd [rcx+disp], xmm0
                    if part_off == 0 {
                        self.emit(&[0x01]);
                    } else if part_off.abs() < 128 {
                        self.emit(&[0x41, part_off as u8]);
                    } else {
                        self.emit(&[0x81]);
                        self.emit(&(part_off as i32).to_le_bytes());
                    }
                } else {
                    // 整数分量: 值在 rax, 按内型宽度存储
                    if part_off != 0 {
                        self.emit(&[0x48, 0x81, 0xC1]); // add rcx, imm32
                        self.emit(&(part_off as i32).to_le_bytes());
                    }
                    match inner.size() {
                        1 => self.emit(&[0x88, 0x01]),
                        2 => self.emit(&[0x66, 0x89, 0x01]),
                        4 => self.emit(&[0x89, 0x01]),
                        _ => self.emit(&[0x48, 0x89, 0x01]),
                    }
                }
            } else {
                // 非 lvalue (如 __imag__ (a+b)): 不可赋值, 仅求值右操作数
                self.gen_expr(r, 0);
            }
        } else {
            // 通用 lvalue 回退: 用于 Cast-lvalue 或未来
            // 新增的 lvalue 形式 (如复合表达式解引用).
            // 通过 gen_lvalue_addr 计算目标地址, 再按类型
            // 宽度发射 store 指令. 避免未匹配任何分支时
            // 赋值静默变成 no-op.
            let assign_sz = assign_l_ty.size();
            let is_float_ty = matches!(assign_l_ty, Type::Float | Type::Double | Type::LongDouble);
            let is_aggregate = matches!(
                assign_l_ty,
                Type::Struct(_, _, _, _)
                    | Type::Union(_, _, _, _)
                    | Type::Array(_, _)
                    | Type::Vector(_, _)
            ) && assign_sz > 8;
            // _Complex 浮点赋值到 Index/Deref/Cast lvalue: 取目标
            // 地址后逐分量写入 (实部 + 虚部)。RHS 的虚部按 complex
            // rvalue 约定定位 (gen_builtin 内联物化/binop 在临时槽,
            // SysV SSE 返回在 xmm1, 标量为 0) — 交由
            // gen_complex_imag_into_xmm0 统一判定。
            if matches!(assign_l_ty, Type::Complex(_)) && !is_aggregate {
                if let Type::Complex(ref inner) = assign_l_ty {
                    if matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble) {
                        if self.gen_lvalue_addr(lv, 0) {
                            self.push(0); // 保存目标地址 (gen_expr 破坏 rax)
                            self.gen_expr(r, 0); // xmm0 = 实部
                            self.pop(1); // rcx = 目标地址
                            self.emit_scalar_to_xmm0_if_needed(r, &inner.base);
                            let pfx = if matches!(inner.base, Type::Float) {
                                0xF3u8
                            } else {
                                0xF2u8
                            };
                            let inner_sz = inner.size() as i64;
                            // 实部 → [rcx]
                            self.emit(&[pfx, 0x0F, 0x11, 0x01]);
                            // 虚部 → xmm0 → [rcx+inner_sz]
                            self.gen_complex_imag_into_xmm0(r, &inner.base);
                            self.emit(&[pfx, 0x0F, 0x11, 0x41]);
                            self.emit(&[inner_sz as u8]);
                            return;
                        } else {
                            // 无法取地址: 仅求值 RHS
                            self.gen_expr(r, 0);
                            return;
                        }
                    }
                }
            }
            if is_aggregate {
                // 聚合类型 (>8B): 取地址 + rep movsb 拷贝
                if self.gen_lvalue_addr(lv, 0) {
                    self.push(0); // 保存目标地址
                    self.gen_expr(r, 0);
                    // 源地址: gen_expr 对聚合类型结果在帧临时区
                    let src_off = -self.frame_size + 8;
                    self.emit_lea_r13(6, src_off); // rsi = src
                    self.pop(7); // rdi = dst
                    self.emit(&[0xFC]); // cld
                    self.mov_imm(2, assign_sz as i128); // rcx = size
                    self.emit(&[0xF3, 0xA4]); // rep movsb
                } else {
                    self.gen_expr(r, 0);
                }
            } else if self.gen_lvalue_addr(lv, 0) {
                self.push(0); // 保存目标地址
                self.gen_expr(r, 0); // rax = val (浮点: xmm0)
                self.pop(1); // rcx = 目标地址
                if is_float_ty {
                    self.emit_store_scalar_float_at(&r.ty(), &assign_l_ty, 1); // 单一真源：收浮影子副本
                } else {
                    match assign_sz {
                        1 => self.emit(&[0x88, 0x01]),       // mov [rcx], al
                        2 => self.emit(&[0x66, 0x89, 0x01]), // mov [rcx], ax
                        4 => self.emit(&[0x89, 0x01]),       // mov [rcx], eax
                        _ => self.emit(&[0x48, 0x89, 0x01]), // mov [rcx], rax
                    }
                }
            } else {
                // gen_lvalue_addr 失败: 无法取地址, 仅求值 RHS
                self.gen_expr(r, 0);
            }
        }
    }
}
