// src/codegen/x86_64/emit/expr/cast.rs
// Expr::Cast arm 的 codegen (gen_expr 中的显式类型转换分支, 含 vector
// 复合字面量、同宽 bitcast、指针/数组 decay、整数/浮点窄扩展等)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4f): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_cast`, 语义 diff 为 0。旧影子 `gen_cast_expr` 自由函数一并作废。

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::Codegen;

impl Codegen {
    /// gen_expr 中 Expr::Cast arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_cast(
        &mut self,
        ty: &Type,
        e: &Box<Expr>,
        dst: u8,
    ) {
                // (v4si){...} 复合字面量：parser 降级为 Cast + InitList
                if let (Type::Vector(elem_ty, n), Expr::InitList { inits, .. }) = (ty, e.as_ref()) {
                    self.gen_vector_init_rvalue(inits, &elem_ty.base, *n, 0);
                    // rax 指向临时槽, 供后续 aggregate_assign 或向量操作使用
                    let vec_size = elem_ty.base.size() * n;
                    let temp_off = self.vector_temp_base(0, vec_size);
                    self.emit_lea_r13(0, temp_off);
                    return;
                }
                let src_ty = e.ty();
                // 标量整数→向量 (同尺寸): 直接用 movq 将 GP 寄存器值移入 XMM
                // 然后存到临时槽, 供后续 aggregate assign 使用
                if let Type::Vector(_, _) = ty {
                    if src_ty.size() == ty.size()
                        && src_ty.size() <= 8
                        && src_ty.is_int()
                    {
                        self.gen_expr(e, dst);
                        if dst != 0 {
                            self.mov(0, dst);
                        }
                        // movq xmm0, rax: 66 REX.W 0F 6E C0
                        self.emit_movq_gp_to_xmm(0, 0);
                        // 存 xmm0 到临时槽, 供后续 aggregate assign 从内存读
                        let vec_size = ty.size();
                        let temp_off = self.vector_temp_base(0, vec_size);
                        // movq [r13+temp_off], xmm0: 66 REX.B 0F D6 /r (store)
                        // 位移可能超出 8 位, 用 mod=10 (32-bit disp)
                        // ModRM: mod=10, reg=xmm0(000), r/m=100(SIB)
                        // SIB: scale=00, index=100(none), base=101(r13) + REX.B
                        let rex = 0x41; // REX.B for r13
                        self.emit(&[0x66, rex, 0x0F, 0xD6, 0x84, 0x25]);
                        self.emit(&(temp_off as i32).to_le_bytes());
                        // rax = 临时槽地址 (供后续使用)
                        self.emit_lea_r13(0, temp_off);
                        return;
                    }
                }
                // 向量→标量整数 (同尺寸): 从向量内存加载到 GP 寄存器
                // 关键: vector 可能 < 8B, 不能固定 movq 读 8 字节 (读到垃圾).
                // 改为按 vector 实际 size 直接 load 到 GP, 再 zero-extend 到 64 位.
                if let Type::Vector(_, _) = src_ty {
                    if src_ty.size() == ty.size()
                        && ty.size() <= 8
                        && ty.is_int()
                    {
                        // 取 vector 地址到 rax (rvalue 先物化到临时槽)
                        if !self.gen_lvalue_addr(e, 0) {
                            self.gen_expr(e, 0);
                            let toff = self.vector_temp_base(0, src_ty.size());
                            self.emit_lea_r13(0, toff);
                        }
                        // rax = vector 内存地址, 按实际 size 加载到 rax
                        let sz = src_ty.size();
                        match sz {
                            1 => {
                                // movzx eax, byte [rax]
                                self.emit(&[0x0F, 0xB6, 0x00]);
                            }
                            2 => {
                                // movzx eax, word [rax]
                                self.emit(&[0x0F, 0xB7, 0x00]);
                            }
                            4 => {
                                // mov eax, dword [rax] (自动 zero-extend 到 64 位)
                                self.emit(&[0x8B, 0x00]);
                            }
                            8 => {
                                // mov rax, qword [rax] (64 位无 truncation)
                                self.emit(&[0x48, 0x8B, 0x00]);
                            }
                            _ => {
                                // 更大? 不可能, 条件已限定 <= 8
                                self.emit(&[0x48, 0x8B, 0x00]);
                            }
                        }
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return;
                    }
                }
                if self.gen_same_size_bitcast(e, ty, dst) {
                    return;
                }
                // 数组在 cast 中 decay 为首元素地址, 如 (const __m128i *) buf.
                // 直接 gen_expr 会误读数组内容 (mov 而非 lea).
                if matches!(src_ty, Type::Array(_, _)) && self.gen_lvalue_addr(e, dst) {
                    return;
                }
                self.gen_expr(e, dst);
                // 类型转换
                match (&src_ty, ty) {
                    (src, Type::Int128 | Type::UInt128)
                        if !matches!(src, Type::Int128 | Type::UInt128) =>
                    {
                        if dst != 0 {
                            self.mov(0, dst);
                        }
                        if matches!(
                            src,
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                        ) {
                            self.emit(&[0x48, 0x31, 0xD2]);
                        } else {
                            self.emit(&[0x48, 0x99]);
                        }
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                    }
                    // unsigned → signed same-size integer cast:
                    // The value in dst is zero-extended (from loading
                    // unsigned), but the signed consumer (e.g. idiv)
                    // needs sign-extension.  Emit movsxd / movswx /
                    // movsbw to re-interpret.
                    (Type::UInt, Type::Int) => {
                        // movsxd dst, dst (sign-extend 32→64)
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x63, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                    }
                    // signed → unsigned same-size: 截断到类型宽度并零扩展
                    // 如 (unsigned int)(signed_expr) 需要确保高位为 0
                    (Type::Int, Type::UInt) | (Type::Long, Type::ULong) | (Type::LongLong, Type::ULongLong) => {
                        // mov dst32, dst32 (零扩展 32→64) 或 mov dst64, dst64
                        let sz = ty.size();
                        if sz <= 4 {
                            let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
                            self.emit(&[rex, 0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        }
                        // 64-bit: 无需处理 (已经是 64 位)
                    }
                    (Type::UShort, Type::Short) => {
                        // movswl dst, dst (sign-extend 16→32, zero-extend to 64)
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                    }
                    (Type::UChar, Type::SChar | Type::Char) => {
                        // movsbl dst, dst (sign-extend 8→32, zero to 64)
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                    }
                    // int → float/double
                    // 源整数值由 gen_expr(e, dst) 求值到 dst 寄存器,
                    // 转换指令必须读取 dst (而非固定 rax), 否则嵌套在
                    // Binop 右操作数 (dst=rcx 等) 时会读到错误寄存器.
                    (src @ (Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short), Type::Float) => {
                        // cvtsi2ss xmm0, dst (F3 0F 2A /r)
                        let wide = matches!(src, Type::Long | Type::LongLong);
                        let rex = 0x40
                            | (if wide { 0x08 } else { 0 })
                            | (if dst >= 8 { 0x01 } else { 0 });
                        self.emit(&[0xF3, rex, 0x0F, 0x2A, 0xC0 | (dst & 7)]);
                    }
                    (
                        src @ (Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short),
                        Type::Double | Type::LongDouble,
                    ) => {
                        // cvtsi2sd xmm0, dst (F2 0F 2A /r)
                        let wide = matches!(src, Type::Long | Type::LongLong);
                        let rex = 0x40
                            | (if wide { 0x08 } else { 0 })
                            | (if dst >= 8 { 0x01 } else { 0 });
                        self.emit(&[0xF2, rex, 0x0F, 0x2A, 0xC0 | (dst & 7)]);
                    }
                    // float/double → int
                    // 浮点值按约定在 xmm0, 转换结果必须写入 dst 寄存器,
                    // 否则嵌套在 Binop 右操作数 (dst=rcx) 时结果被覆盖.
                    (Type::Float, Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short) => {
                        // cvttss2si dst, xmm0 (F3 0F 2C /r)
                        let wide = matches!(ty, Type::Long | Type::LongLong);
                        let rex = 0x40
                            | (if wide { 0x08 } else { 0 })
                            | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[0xF3, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]);
                        self.emit_float_to_int_saturate(dst, &Type::Float, wide);
                    }
                    (
                        Type::Double,
                        Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short,
                    ) => {
                        // cvttsd2si dst, xmm0 (F2 0F 2C /r)
                        let wide = matches!(ty, Type::Long | Type::LongLong);
                        let rex = 0x40
                            | (if wide { 0x08 } else { 0 })
                            | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[0xF2, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]);
                        self.emit_float_to_int_saturate(dst, &Type::Double, wide);
                    }
                    (
                        Type::LongDouble,
                        Type::Int | Type::Long | Type::LongLong | Type::Char | Type::Short,
                    ) => {
                        // long double 内部为 double, 值已在 xmm0
                        // cvttsd2si dst, xmm0
                        let wide = matches!(ty, Type::Long | Type::LongLong);
                        let rex = 0x40
                            | (if wide { 0x08 } else { 0 })
                            | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[0xF2, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]);
                        self.emit_float_to_int_saturate(dst, &Type::LongDouble, wide);
                    }
                    (Type::Complex(inner), Type::Int | Type::Long | Type::Char | Type::Short) => {
                        let is_f32 = matches!(&inner.base, Type::Float);
                        match e.as_ref() {
                            Expr::Binop { .. } => {
                                // gen_complex_binop 已将实部留在 xmm0
                            }
                            Expr::Ident { name, .. } => {
                                let off = self.var_offset(&name.name);
                                let pfx = if is_f32 { 0xF3u8 } else { 0xF2u8 };
                                self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x10], off);
                            }
                            _ => {
                                let result_off = -self.frame_size + 8;
                                let pfx = if is_f32 { 0xF3u8 } else { 0xF2u8 };
                                self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x10], result_off);
                            }
                        }
                        if is_f32 {
                            self.emit(&[0xF3, 0x0F, 0x2C, 0xC0]);
                        } else {
                            self.emit(&[0xF2, 0x0F, 0x2C, 0xC0]);
                        }
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                    }
                    // float ↔ double
                    (Type::Float, Type::Double | Type::LongDouble) => {
                        // cvtss2sd xmm0, xmm0 (F3 0F 5A C0)
                        self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]);
                    }
                    (Type::Double | Type::LongDouble, Type::Float) => {
                        // cvtsd2ss xmm0, xmm0 (F2 0F 5A C0)
                        self.emit(&[0xF2, 0x0F, 0x5A, 0xC0]);
                    }
                    // unsigned int (<=32bit) → float/double:
                    // 零扩展到 64 位后值一定落在 signed int64 正半轴,
                    // 可直接用 cvtsi2ss/cvtsi2sd (与 signed 路径相同指令).
                    (
                        src @ (Type::Bool | Type::UChar | Type::UShort | Type::UInt),
                        Type::Float,
                    ) => {
                        // 零扩展: 截断到源宽度, 高 32 位自动清零
                        self.emit_zero_extend(dst, src);
                        // cvtsi2ss xmm0, dst (F3 REX.W 0F 2A /r)
                        // 需要 REX.W: UInt 零扩展后值可能 > INT32_MAX,
                        // 必须以 64 位有符号解释.
                        let rex = 0x48 | (if dst >= 8 { 0x01 } else { 0 });
                        self.emit(&[0xF3, rex, 0x0F, 0x2A, 0xC0 | (dst & 7)]);
                    }
                    (
                        src @ (Type::Bool | Type::UChar | Type::UShort | Type::UInt),
                        Type::Double | Type::LongDouble,
                    ) => {
                        self.emit_zero_extend(dst, src);
                        // cvtsi2sd xmm0, dst (F2 REX.W 0F 2A /r)
                        let rex = 0x48 | (if dst >= 8 { 0x01 } else { 0 });
                        self.emit(&[0xF2, rex, 0x0F, 0x2A, 0xC0 | (dst & 7)]);
                    }
                    // unsigned long/longlong → float/double:
                    // 64 位无符号值可能超过 signed int64 范围.
                    // 策略: 检查符号位, 正数直接 cvtsi2ss/sd;
                    // 负数 (bit63=1) 先 shr 1, 转换后乘 2.0.
                    (Type::ULong | Type::ULongLong, Type::Float) => {
                        self.emit_ulong_to_float(dst, false);
                    }
                    (Type::ULong | Type::ULongLong, Type::Double | Type::LongDouble) => {
                        self.emit_ulong_to_float(dst, true);
                    }
                    // float/double → unsigned int (<=32bit):
                    // 先用 cvttss2si/cvttsd2si 转到 signed int64,
                    // 负值 (float<0) 钳位到 0, 然后截断到目标宽度.
                    (Type::Float, Type::Bool | Type::UChar | Type::UShort | Type::UInt) => {
                        // cvttss2si dst, xmm0 (REX.W 64 位结果)
                        let rex = 0x48 | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[0xF3, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]);
                        // 负值钳位到 0: 先 zero_reg=0 (xor 会破坏 flags),
                        // 再 test dst, dst 设置 SF, 最后 cmovs 选择.
                        let zero_reg = if dst == 0 { 2 } else { 0 };
                        self.emit(&[0x48, 0x31, 0xC0 | ((zero_reg & 7) << 3) | (zero_reg & 7)]);
                        let test_rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                        self.emit(&[test_rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        let rex_cmov = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[rex_cmov, 0x0F, 0x48, 0xC0 | ((dst & 7) << 3) | (zero_reg & 7)]);
                        // 截断到目标宽度
                        self.emit_truncate_to_unsigned(dst, ty);
                    }
                    (Type::Double, Type::Bool | Type::UChar | Type::UShort | Type::UInt) => {
                        // cvttsd2si dst, xmm0 (REX.W 64 位结果)
                        let rex = 0x48 | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[0xF2, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]);
                        let zero_reg = if dst == 0 { 2 } else { 0 };
                        self.emit(&[0x48, 0x31, 0xC0 | ((zero_reg & 7) << 3) | (zero_reg & 7)]);
                        let test_rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                        self.emit(&[test_rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        let rex_cmov = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[rex_cmov, 0x0F, 0x48, 0xC0 | ((dst & 7) << 3) | (zero_reg & 7)]);
                        self.emit_truncate_to_unsigned(dst, ty);
                    }
                    (Type::LongDouble, Type::Bool | Type::UChar | Type::UShort | Type::UInt) => {
                        // long double 内部为 double, 值已在 xmm0
                        let rex = 0x48 | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[0xF2, rex, 0x0F, 0x2C, 0xC0 | ((dst & 7) << 3)]);
                        let zero_reg = if dst == 0 { 2 } else { 0 };
                        self.emit(&[0x48, 0x31, 0xC0 | ((zero_reg & 7) << 3) | (zero_reg & 7)]);
                        let test_rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                        self.emit(&[test_rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        let rex_cmov = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[rex_cmov, 0x0F, 0x48, 0xC0 | ((dst & 7) << 3) | (zero_reg & 7)]);
                        self.emit_truncate_to_unsigned(dst, ty);
                    }
                    // float/double → ulong/ulonglong:
                    // cvttss2si/cvttsd2si 结果若为非负即正确;
                    // 若为负 (说明原值 > INT64_MAX), 加 2^64 校正.
                    (Type::Float, Type::ULong | Type::ULongLong) => {
                        self.emit_float_to_ulong(dst, false);
                    }
                    (Type::Double, Type::ULong | Type::ULongLong) => {
                        self.emit_float_to_ulong(dst, true);
                    }
                    (Type::LongDouble, Type::ULong | Type::ULongLong) => {
                        // long double 内部为 double, 值已在 xmm0
                        self.emit_float_to_ulong(dst, true);
                    }
                    // 整数窄化转换: 源类型宽度 > 目标类型宽度时需要截断
                    (src, dst_ty) if src.size() > dst_ty.size() => {
                        // 目标为无符号小类型: 截断到目标宽度 (零扩展语义)
                        // 目标为有符号小类型: 截断后符号扩展
                        match dst_ty {
                            Type::Bool | Type::UChar => {
                                // and dst, 0xFF
                                let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                                self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                                self.emit(&0xFFu32.to_le_bytes());
                            }
                            Type::Char | Type::SChar | Type::Short => {
                                // 截断到 8/16 位后符号扩展
                                let bits = dst_ty.size() * 8;
                                if bits == 8 {
                                    // movsx dst, dst_byte (REX.W 0F BE C0)
                                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0x40 };
                                    self.emit(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                } else {
                                    // movsx dst, dst_word (REX.W 0F BF C0)
                                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0x40 };
                                    self.emit(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                }
                            }
                            Type::UShort => {
                                // and dst, 0xFFFF
                                let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                                self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                                self.emit(&0xFFFFu32.to_le_bytes());
                            }
                            Type::UInt => {
                                // mov eax, eax (32位零扩展到64位)
                                let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
                                self.emit(&[rex, 0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            }
                            Type::Int => {
                                // movsxd dst, dst (sign-extend 32→64)
                                // 截断到 32 位后符号扩展, 供后续有符号运算
                                // (如 idiv, cmp) 使用正确的 64 位值.
                                let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                                self.emit(&[rex, 0x63, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            }
                            _ => {}
                        }
                    }
                    // 整数缩小转换: 截断到目标类型宽度并符号/零扩展.
                    // 例如 (char)0xff: Int 值 255 需截断为 8 位再符号扩展为 -1.
                    // 前面已处理的特定转换 (Int128/浮点/复数等) 不会到达此处.
                    (src, dst_ty) if Self::is_int_narrow(src, dst_ty) => {
                        self.gen_int_narrow_cast(dst, dst_ty);
                    }
                    // 整数宽度提升但符号性改变 (sign-change widening).
                    // 小类型 (< int) 在寄存器中以符号/零扩展形式存在于 64 位中.
                    // 当 Cast 到不同符号性的类型时, 需重新做扩展.
                    (src, dst_ty)
                        if Self::is_integer_type(src)
                            && Self::is_integer_type(dst_ty)
                            && src.size() < 4 =>
                    {
                        let src_signed = Self::is_signed_int(src)
                            || matches!(src, Type::Char);
                        let dst_signed = Self::is_signed_int(dst_ty)
                            || matches!(dst_ty, Type::Char);
                        if src_signed && !dst_signed {
                            // 有符号源 → 无符号目标: 寄存器中是符号扩展值,
                            // 需截断到目标宽度 (零扩展语义).
                            let dst_bits = dst_ty.size() * 8;
                            match dst_bits {
                                8 => {
                                    // movzbl %al, %eax (截断到 8 位, 零扩展到 32 位)
                                    let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
                                    self.emit(&[rex, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                }
                                16 => {
                                    // movzwl %ax, %eax (截断到 16 位, 零扩展到 32 位)
                                    let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
                                    self.emit(&[rex, 0x0F, 0xB7, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                }
                                32 => {
                                    // mov dst32, dst32 (截断到 32 位, 零扩展到 64 位)
                                    let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
                                    self.emit(&[rex, 0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                }
                                _ => {}
                            }
                        } else if !src_signed && dst_signed && dst_ty.size() <= src.size() {
                            // 无符号源 → 有符号目标, 目标不比源宽:
                            // 寄存器中是零扩展值, 需截断到目标宽度并符号扩展.
                            let dst_bits = dst_ty.size() * 8;
                            match dst_bits {
                                8 => {
                                    // movsbq %al, %rax (截断到 8 位, 符号扩展)
                                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                                    self.emit(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                }
                                16 => {
                                    // movswq %ax, %rax (截断到 16 位, 符号扩展)
                                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                                    self.emit(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                                }
                                _ => {}
                            }
                        }
                        // 注: 无符号源 → 有符号目标, 目标更宽时, 零扩展值
                        // 已正确表示非负有符号值, 无需转换.
                    }
                    // 其他情况: 不转换 (同类型/指针/聚合等无需转换)
                    _ => {}
                }
    }
}
