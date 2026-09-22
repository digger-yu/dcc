// src/codegen/x86_64/emit/expr/binary.rs
// Expr::Binop arm 的 codegen (gen_expr 中的二元运算分支, 含常量折叠 / 短路 / 
// 浮点 / __int128 / Vector / Complex / 指针算术 / 位域)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4c): arm body 原样剪切进 `impl Codegen`
// 新方法 `gen_expr_binop`, 语义 diff 为 0。旧影子 `gen_binop_expr` 自由函数及
// 相关 helper (emit_int_binop / emit_ptr_binop / ...) 一并作废。

use crate::ast::{BinOp, Expr, Type};
use crate::codegen::x86_64::{Codegen, JumpKind};
use crate::const_eval;

impl Codegen {
    /// gen_expr 中 Expr::Binop arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_binop(
        &mut self,
        e: &Expr,
        op: &BinOp,
        l: &Box<Expr>,
        r: &Box<Expr>,
        binop_ty: &Type,
        dst: u8,
    ) {
                // 常量折叠: 若两个操作数都是整数常量, 直接在编译期求值,
                // 避免为 `1 << 6` 等常量表达式生成运行时计算代码.
                // 这也能规避嵌套 BinOp 在特定寄存器分配下的 codegen bug.
                // 折叠仅限整型 Binop: 浮点/复数/向量运算必须走下方 SSE
                // 路径。const_eval::eval 是整型 API, 会忽略 fenv_access 并
                // 把浮点结果截断为整数 (1.0/3.0 → 0); FENV_ACCESS ON 时
                // 浮点运算须按运行时舍入模式求值。浮点常量折叠由 opt.rs
                // 在 fenv_access=OFF 时于优化阶段完成。
                if !matches!(op, BinOp::AndAnd | BinOp::OrOr)
                    && Self::is_integer_type(&l.ty())
                    && Self::is_integer_type(&r.ty())
                    && Self::is_integer_type(binop_ty)
                {
                    if let Ok(val) = const_eval::eval(e) {
                        self.mov_imm(dst, val);
                        return;
                    }
                }
                // 逻辑与/或优先于浮点路径：比较结果为 int，但操作数可为浮点
                if matches!(op, BinOp::AndAnd | BinOp::OrOr) {
                    let l_false = self.fresh_label();
                    let l_end = self.fresh_label();
                    self.gen_expr_decay(l, dst);
                    // P1-4: int128 操作数的高 64 位在 rdx, 与 dst(low) 或合并后
                    // 再 test, 否则高 1 低 0 的 __int128 会被误判为 0.
                    if matches!(l.ty(), Type::Int128 | Type::UInt128) {
                        let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
                    }
                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                    self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                    match op {
                        BinOp::AndAnd => {
                            self.jmp_rel32(&l_false, JumpKind::Je);
                            self.gen_expr_decay(r, dst);
                            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                                let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                                self.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
                            }
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            // setne dst_low: 结果写入 dst 的低 8 位, 而非固定 al
                            self.emit(&[0x0F, 0x95, 0xC0 | (dst & 7)]);
                            // movzb dst, dst_low: 零扩展 dst 低 8 位到整个 dst
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            self.mov_imm(dst, 0);
                            self.label(&l_end);
                        }
                        BinOp::OrOr => {
                            self.jmp_rel32(&l_false, JumpKind::Jne);
                            self.gen_expr_decay(r, dst);
                            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                                let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                                self.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
                            }
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            // setne dst_low: 结果写入 dst 的低 8 位, 而非固定 al
                            self.emit(&[0x0F, 0x95, 0xC0 | (dst & 7)]);
                            // movzb dst, dst_low: 零扩展 dst 低 8 位到整个 dst
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            self.mov_imm(dst, 1);
                            self.label(&l_end);
                        }
                        _ => unreachable!(),
                    }
                    return;
                }
                // 浮点运算: 使用 SSE 指令；FP_CONTRACT ON 时允许 a*b+c 收缩.
                // Check BOTH operands: `int - float` must go through float path.
                // _Complex/_Imaginary 操作数必须走下方 gen_complex_binop /
                // gen_imaginary_binop 统一约定 (两分量求值并写回 result_off
                // 内存对 + xmm0=实部 + rax=result_off)。若放行到标量路径,
                // gen_float_binop 只算实部且不写回物化槽 (rax 指向的内存对
                // 实部是陈旧值), spill/传参/比较按内存对读取时实部错误
                // (complex-2: a != 1.0+1.0i 恒为真)。
                if !matches!(l.ty(), Type::Complex(_) | Type::Imaginary(_))
                    && !matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_))
                    && !matches!(l.ty(), Type::Vector(_, _))
                    && !matches!(r.ty(), Type::Vector(_, _))
                    && (matches!(l.ty(), Type::Float | Type::Double | Type::LongDouble)
                        || matches!(r.ty(), Type::Float | Type::Double | Type::LongDouble))
                {
                    if self.fp_contract && self.try_gen_fp_contract(*op, l, r, dst) {
                        return;
                    }
                    self.gen_float_binop(*op, l, r);
                    // 浮点比较结果在 rax, 需要移动到 dst
                    if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne) {
                        if dst != 0 {
                            self.mov(0, dst);
                        }
                    } else if Self::is_integer_type(binop_ty) {
                        // 复合赋值脱糖形态: `int a += double d` 的 Binop.ty
                        // 标注为 LHS 整数类型 (parser.rs C99 6.5.17 计算后回
                        // 转语义), 但 gen_float_binop 的混合运算结果在 xmm0
                        // — 必须转回整数, 否则 Assign 层按 ty=整数直接存
                        // dst 中的陈旧值 (mix.c: a+=d 得 1 而非 20)。
                        // 普通表达式 `a + d` 的 ty 是通常算术转换类型
                        // (浮点), 不受影响。
                        let fl_ty = if matches!(l.ty(), Type::LongDouble)
                            || matches!(r.ty(), Type::LongDouble)
                        {
                            Type::LongDouble
                        } else if matches!(l.ty(), Type::Double)
                            || matches!(r.ty(), Type::Double)
                        {
                            Type::Double
                        } else {
                            Type::Float
                        };
                        self.emit_implicit_float_to_int(dst, &fl_ty, binop_ty);
                    }
                    return;
                }
                // SIMD 向量运算: 标量逐元素，结果写入帧临时区
                if matches!(l.ty(), Type::Vector(_, _))
                    || matches!(r.ty(), Type::Vector(_, _))
                {
                    self.gen_vector_binop(*op, l, r, 0);
                    return;
                }
                // P1-4: __int128 运算
                if matches!(l.ty(), Type::Int128 | Type::UInt128)
                    || matches!(r.ty(), Type::Int128 | Type::UInt128)
                {
                    self.gen_int128_binop(*op, l, r, dst);
                    return;
                }
                // P2-14: _Complex 算术运算 (任一操作数为复数, 标量隐式提升)
                if matches!(l.ty(), Type::Complex(_)) || matches!(r.ty(), Type::Complex(_)) {
                    self.gen_complex_binop(*op, l, r, dst);
                    return;
                }
                // _Imaginary 算术运算
                if matches!(l.ty(), Type::Imaginary(_)) || matches!(r.ty(), Type::Imaginary(_)) {
                    self.gen_imaginary_binop(*op, l, r, dst);
                    return;
                }
                // 指针/数组运算缩放: ptr ± n 步进 sizeof(*ptr), ptr - ptr 返回元素数
                // 数组衰减为指针: Array(T,_) 视为 Ptr(T)
                let l_inner = match l.ty() {
                    Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()),
                    _ => None,
                };
                let r_inner = match r.ty() {
                    Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()),
                    _ => None,
                };
                let is_ptr_sub = matches!(op, BinOp::Sub) && l_inner.is_some() && r_inner.is_some();
                let needs_scale = matches!(op, BinOp::Add | BinOp::Sub) && !is_ptr_sub;
                let scale = if needs_scale {
                    if let Some(s) = l_inner {
                        s
                    } else {
                        r_inner.unwrap_or(1)
                    }
                } else {
                    1
                };
                let elem_size = if is_ptr_sub { l_inner.unwrap_or(1) } else { 1 };
                // C11 6.3.1.8 通常算术转换: 确定运算的有符号性.
                // C11 6.3.1.1: 整数提升 (char/short → int) 必须在
                // 有符号性判定之前执行. 例如 signed char vs unsigned char
                // 提升后均为 int (有符号), 比较应使用有符号指令.
                let int_promote = |ty: &Type| -> Type {
                    match ty {
                        Type::Bool | Type::Char | Type::SChar | Type::UChar
                        | Type::Short | Type::UShort => Type::Int,
                        _ => ty.clone(),
                    }
                };
                let lt_promo = int_promote(&l.ty());
                let rt_promo = int_promote(&r.ty());
                let type_rank = |ty: &Type| -> i32 {
                    match ty {
                        Type::Bool | Type::Char | Type::SChar | Type::UChar => 1,
                        Type::Short | Type::UShort => 2,
                        Type::Int | Type::UInt => 3,
                        Type::Long | Type::ULong => 4,
                        Type::LongLong | Type::ULongLong => 5,
                        Type::Int128 | Type::UInt128 => 6,
                        _ => 0,
                    }
                };
                let ls = Self::is_signed_int(&lt_promo);
                let rs = Self::is_signed_int(&rt_promo);
                // C11 6.5.7: 移位运算的有符号性仅由左操作数决定,
                // 右操作数仅为移位计数, 不参与符号判定.
                let is_signed = if matches!(op, BinOp::Shl | BinOp::Shr) {
                    Self::is_signed_int(&l.ty())
                } else if ls == rs {
                    ls // 两操作数同符号
                } else {
                    // 混合符号: 有符号类型秩严格大于无符号类型秩 → 有符号
                    let lr = type_rank(&lt_promo);
                    let rr = type_rank(&rt_promo);
                    if ls && lr > rr { true }
                    else { rs && rr > lr }
                };
                // 统一路径: 被调用者保存寄存器优先, 帧临时槽后备.
                // 使用 gen_expr_decay: 数组标识符在运算中衰减为指针
                // binop 临时槽: 放在 callee-saved 区域下方
                // 布局: r13+0 saved r13 / r13+8 ret addr
                //       / r13-16..r13-48 callee-saved save area (rbx/r12/r14/r15)
                //       / r13-48..-(48+temp*8) binop temp_slots (槽 d 在 -(48+d*8))
                //       / r13-(48+temp*8).. 变量 (基址 hdr 含 temp_slots)
                // 注意: binop 槽公式只用 48 (callee-saved 区大小),
                // temp_slots 只体现在变量基址 hdr 中, 不可重复计入.
                let slot_off = -(48 + self.expr_depth as i32 * 8);
                // 尝试使用被调用者保存寄存器代替帧槽 (减少内存访问).
                // callee-saved 跨 call 保活 (ABI 保证 + 本编译器序言保存
                // callee_saved_used 集合), 因此含 call 的表达式同样安全;
                // 递归 binop 通过 reg_allocated 位图分配, 不冲突.
                // dst==1 (rcx) 时左值可能是 shift 计数, 保守退回帧槽.
                let use_callee_saved = dst != 1;
                let callee_reg = if use_callee_saved {
                    self.alloc_callee_saved()
                } else {
                    None
                };
                self.gen_expr_decay(l, dst);
                // 保存左操作数: 优先到被调用者保存寄存器, 否则到帧槽
                if let Some(reg) = callee_reg {
                    self.mov(reg, dst);
                } else {
                    // mov [r13+slot_off], dst
                    let mut rex = 0x49u8;
                    if dst >= 8 {
                        rex |= 0x04;
                    }
                    self.emit(&[rex, 0x89]);
                    if slot_off.abs() < 128 {
                        self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]);
                    } else {
                        self.emit(&[0x85 | ((dst & 7) << 3)]);
                        self.emit(&slot_off.to_le_bytes());
                    }
                }
                self.expr_depth += 1;
                self.gen_expr_decay(r, 1);
                self.expr_depth -= 1;
                // 对整数操作数乘以指针所指类型大小 (跳过 char* 和 ptr-ptr)
                if scale > 1 {
                    // 有符号 32 位或更小整数需先符号扩展到 64 位, 否则
                    // 负值零扩展后成为大正数, 乘积错误 (pr44575 类 bug)
                    let r_ty = r.ty();
                    if Self::is_signed_int(&r_ty) && r_ty.size() <= 4 {
                        // movslq %ecx, %rcx: 48 63 C9
                        self.emit(&[0x48, 0x63, 0xC9]);
                    }
                    self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                    self.emit(&(scale as i32).to_le_bytes());
                }
                if let Some(reg) = callee_reg {
                    // 被调用者保存寄存器路径: 左操作数在 reg, 右操作数在 rcx(1).
                    self.binop(*op, reg, 1, is_signed, &l.ty(), &r.ty());
                    if reg != dst {
                        self.mov(dst, reg);
                    }
                    self.free_callee_saved(reg);
                } else if dst == 1 {
                    // 帧槽路径, dst == rcx: push/pop 避免恢复覆盖右操作数
                    self.push(1); // push rcx (右操作数)
                    // 恢复左操作数到 dst
                    {
                        let mut rex = 0x49u8;
                        if dst >= 8 {
                            rex |= 0x04;
                        }
                        self.emit(&[rex, 0x8B]);
                        if slot_off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&slot_off.to_le_bytes());
                        }
                    }
                    self.pop(2); // pop rdx (右操作数)
                    self.binop(*op, dst, 2, is_signed, &l.ty(), &r.ty());
                } else {
                    // 帧槽路径: 恢复左操作数
                    // mov dst, [r13+slot_off]
                    {
                        let mut rex = 0x49u8;
                        if dst >= 8 {
                            rex |= 0x04;
                        }
                        self.emit(&[rex, 0x8B]);
                        if slot_off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&slot_off.to_le_bytes());
                        }
                    }
                    self.binop(*op, dst, 1, is_signed, &l.ty(), &r.ty());
                }
                // ptr - ptr: 字节差 ÷ 元素大小 = 元素数.
                // idiv 固定以 rax 为被除数, 而 binop 结果在 dst:
                // dst != 0 时须先搬 rax, 除完再把商搬回 dst.
                if is_ptr_sub && elem_size > 1 {
                    if dst != 0 {
                        self.mov(0, dst); // rax = 字节差
                    }
                    self.mov_imm(1, elem_size as i128); // rcx = elem_size
                    self.emit(&[0x48, 0x99]); // cqo (符号扩展 rax→rdx:rax)
                    self.emit(&[0x48, 0xf7, 0xf9]); // idiv rcx
                    if dst != 0 {
                        self.mov(dst, 0); // dst = 商
                    }
                }
                // C 语义: 32 位整数运算结果需截断回 32 位 (溢出 wrap),
                // 否则 64 位寄存器高位垃圾会泄漏给后续移位/除法等运算.
                {
                    let res_ty = e.ty();
                    if res_ty.size() == 4
                        && res_ty.is_int()
                        && matches!(*op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Shl)
                    {
                        if Self::is_signed_int(&res_ty) {
                            // movsxd dst, dst32 (符号扩展)
                            let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                            self.emit(&[rex, 0x63, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        } else {
                            // mov dst32, dst32 (零扩展)
                            if dst >= 8 {
                                self.emit(&[0x45]);
                            }
                            self.emit(&[0x8B, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        }
                    }
                }
                // GCC 位域算术语义: 当两个操作数都是位域时, 运算结果掩码到
                // max(width_l, width_r) 位. 当一个操作数是位域而另一个是
                // 整型常量时, 结果掩码到位域宽度.
                // 例如 u33 * u33 → 掩码到 33 位, u40 - 8 → 掩码到 40 位.
                {
                    let bf_l = Self::expr_bitfield_width(l);
                    let bf_r = if matches!(*op, BinOp::Shl | BinOp::Shr) {
                        None // 移位: 仅看左操作数
                    } else {
                        Self::expr_bitfield_width(r)
                    };
                    let r_is_const = matches!(r.as_ref(), Expr::Int { .. });
                    let l_is_const = matches!(l.as_ref(), Expr::Int { .. });
                    // 两个操作数都是位域: 掩码到 max 宽度
                    // 一个位域 + 一个常量: 掩码到位域宽度
                    // 移位 + 左操作数位域: 掩码到位域宽度
                    let effective_width: Option<u32> = match (bf_l, bf_r) {
                        (Some(wl), Some(wr)) => Some(std::cmp::max(wl, wr)),
                        (Some(w), None) if r_is_const || matches!(*op, BinOp::Shl | BinOp::Shr) => Some(w),
                        (None, Some(w)) if l_is_const => Some(w),
                        _ => None,
                    };
                    if let Some(bf_width) = effective_width {
                        if bf_width < 64 && bf_width > 32 {
                            let mask: u64 = if bf_width == 0 { 0 } else { (1u64 << bf_width) - 1 };
                            if mask <= 0x7FFFFFFF {
                                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                                self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                                self.emit(&(mask as u32).to_le_bytes());
                            } else {
                                let tmp = if dst != 1 { 1u8 } else { 2u8 };
                                let rex_tmp = 0x48 | if tmp >= 8 { 0x44 } else { 0 };
                                self.emit(&[rex_tmp, 0xB8 | (tmp & 7)]);
                                self.emit(&mask.to_le_bytes());
                                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 }
                                    | if tmp >= 8 { 0x04 } else { 0 };
                                self.emit(&[rex, 0x21, 0xC0 | ((tmp & 7) << 3) | (dst & 7)]);
                            }
                        }
                    }
                }
                // -ftrapv: 有符号算术溢出检查
                // ADD/SUB/IMUL/IDIV 都会在有符号溢出时置 OF=1, jo 跳到 trap
                if self.trapv
                    && (Self::is_signed_int(&l.ty()) || Self::is_signed_int(&r.ty()))
                    && matches!(*op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                {
                    let trap_label = self.fresh_label();
                    let ok = self.fresh_label();
                    // jo trap_label: 跳到 trap 若 OF=1
                    self.jmp_rel32(&trap_label, JumpKind::Jo);
                    // 未溢出: 跳过 trap 直接到 ok
                    self.jmp_rel32(&ok, JumpKind::Jmp);
                    self.label(&trap_label);
                    self.emit(&[0xe8, 0, 0, 0, 0]); // call rel32
                    let call_at = self.code.len() as u64 - 4;
                    self.pending_func_rels
                        .push((call_at, "__dcc_trapv_overflow".to_string()));
                    self.label(&ok);
                }
    }
}
