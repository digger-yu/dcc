// src/codegen/x86_64/emit/expr/dispatch.rs
// gen_expr 主 dispatcher: 由单体 x86_64.rs L9887-13640 逐字搬迁。

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::{Codegen, JumpKind, VaListAddress};
use crate::const_eval;

impl Codegen {
    pub fn gen_expr(&mut self, e: &Expr, dst: u8) {
        // vector 类型的 lvalue 表达式 (Member/Index/Deref): 物化到向量临时槽 0
        // (dcc vector 值约定: vector rvalue 结果放在临时槽 0)
        if matches!(e, Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. })
            && matches!(e.ty(), Type::Vector(_, _))
        {
            self.gen_vector_load_into_slot(e, 0);
            return;
        }
        // _Complex 类型的 lvalue 表达式 (Member/Index/Deref): 取地址后
        // 逐分量读取, 统一物化到 result_off (dcc complex rvalue 约定:
        // 内存对 + xmm0=实部 + rax=result_off)。旧路径按聚合/标量通用
        // 读取, 对 8B 复数把内容位模式装入 rax, 破坏消费端
        // (spill_complex_operand 的 rep movsb / gen_complex_init)
        // 期望的 rax=地址约定 (20020227-1: packed struct 非对齐
        // y->f != 1 段错误)。
        if matches!(e, Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. })
            && matches!(e.ty(), Type::Complex(_))
            && self.gen_complex_lvalue_rvalue(e)
        {
            return;
        }
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
            Expr::Ident { name, ty, .. } => {
                // 单一入口：本 arm 曾是 `gen_expr_ident_rvalue` 的逐字副本
                // (两份同源代码, 只有这一份在跑)。现只委派。
                self.gen_expr_ident_rvalue(e, name, ty, dst);
            },
            Expr::Binop {
                op,
                l,
                r,
                ty: binop_ty,
                ..
            } => {
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
                        },
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
                        },
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
                    if matches!(
                        op,
                        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne
                    ) {
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
                        } else if matches!(l.ty(), Type::Double) || matches!(r.ty(), Type::Double) {
                            Type::Double
                        } else {
                            Type::Float
                        };
                        self.emit_implicit_float_to_int(dst, &fl_ty, binop_ty);
                    }
                    return;
                }
                // SIMD 向量运算: 标量逐元素，结果写入帧临时区
                if matches!(l.ty(), Type::Vector(_, _)) || matches!(r.ty(), Type::Vector(_, _)) {
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
                        Type::Bool
                        | Type::Char
                        | Type::SChar
                        | Type::UChar
                        | Type::Short
                        | Type::UShort => Type::Int,
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
                    if ls && lr > rr {
                        true
                    } else {
                        rs && rr > lr
                    }
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
                        (Some(w), None) if r_is_const || matches!(*op, BinOp::Shl | BinOp::Shr) => {
                            Some(w)
                        },
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
                                let rex = 0x48
                                    | if dst >= 8 { 0x44 } else { 0 }
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
            },
            Expr::Assign { l, r, .. } => {
                // 检测 compound assignment 模式: Assign { l: target, r: Binop { l: read, r: rhs } }
                // 其中 target 和 read 结构相同 (由 parser desugaring 产生).
                // 如果 lvalue 含副作用 (如 *--ptr += 1), 必须只求值一次.
                // 策略: 先求值 lvalue 地址并保存, 然后从保存的地址读取, 计算 binop, 存回.
                if let Expr::Binop {
                    l: ref binop_l,
                    op: binop_op,
                    r: ref binop_r,
                    ..
                } = **r
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
                let mut lv: &Expr = l.as_ref();
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
                    let rax_is_agg_addr = match **r {
                        Expr::VaArg { ref ty, .. } => Self::va_arg_result_in_memory(ty),
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
                        self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
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
                    let is_aggregate = matches!(
                        l_ty,
                        Type::Struct(_, _, _, _)
                            | Type::Union(_, _, _, _)
                            | Type::Array(_, ArraySize::Const(_))
                            | Type::Atomic(_)
                            | Type::Vector(_, _)
                    ) && (sz > 8
                        || matches!(r.as_ref(), Expr::CompoundLit { .. })
                        || l_ty.has_vla());
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
                        if matches!(l_ty, Type::Float | Type::Double)
                            && matches!(&r_ty, Type::Float | Type::Double)
                        {
                            self.emit_float_precision_convert(&r_ty, l_ty);
                        }
                        // Implicit int→float conversion when RHS is
                        // integer but LHS is float (e.g. `ping = 0;`).
                        // The integer result sits in eax; convert it to
                        // xmm0 before gen_float_store.
                        if matches!(l_ty, Type::Float | Type::Double)
                            && Self::is_integer_type(&r_ty)
                        {
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
                        if l_sz < 4
                            && (Self::is_integer_type(&r_ty) || matches!(r_ty, Type::Enum(_, _, _)))
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
                            let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
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
                                let parent_off =
                                    self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
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
                        } else if self.current_func_is_nested
                            && self.upvar_offsets.contains_key(&name.name)
                        {
                            let parent_off =
                                self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
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
                    use crate::semantics::{
                        compute_lvalue_address, AddressContext, MaterializeAction,
                    };
                    let ctx = AddressContext {
                        is_nested: self.current_func_is_nested,
                    };

                    // 嵌套运行时索引: 退回旧路径 (语义层会丢失第二个运行时偏移)
                    let is_nested_runtime_index = matches!(arr.as_ref(), Expr::Index { .. })
                        && crate::const_eval::eval(idx).is_err()
                        && match arr.as_ref() {
                            Expr::Index { idx: inner_idx, .. } => {
                                crate::const_eval::eval(inner_idx).is_err()
                            },
                            _ => false,
                        };

                    let desc = if !is_nested_runtime_index {
                        Some(compute_lvalue_address(self, lv, &ctx))
                    } else {
                        None
                    };

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
                                self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1);
                            } else {
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
                                self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1);
                            } else {
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
                    let seg_prefix = Self::ptr_seg_prefix(&ptr_e.ty());
                    let is_deref_aggregate = matches!(
                        assign_l_ty,
                        Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _)
                    ) && (assign_l_ty.size() > 8 || assign_l_ty.has_vla());
                    if is_deref_aggregate {
                        // Aggregate *ptr assignment: use rep movsb
                        // to correctly handle packed/misaligned structs.
                        let agg_sz = assign_l_ty.size();
                        // 1. Get source address into rsi
                        let rhs_is_lvalue = matches!(
                            &**r,
                            Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
                        );
                        let rhs_is_ident = matches!(&**r, Expr::Ident { .. });
                        let rhs_is_compound = matches!(&**r, Expr::CompoundLit { .. });
                        let rhs_is_call = matches!(&**r, Expr::Call { ty: ref cty, .. }
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
                            if let Expr::Ident { name, .. } = &**r {
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
                                        // 3. rep movsb
                        if let Some(seg) = seg_prefix {
                            self.emit(&[seg]);
                        }
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
                        self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
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
                            self.pop(1); // rcx = addr                            // __seg_gs/__seg_fs 指针存储: 发射段前缀 (%gs:/%fs:)
                            if let Some(seg) = seg_prefix {
                                self.emit(&[seg]);
                            }
                            if matches!(
                                &assign_l_ty,
                                Type::Float | Type::Double | Type::LongDouble
                            ) {
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
                                Type::Struct(_, _, _, _)
                                    | Type::Union(_, _, _, _)
                                    | Type::Array(_, _)
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
                            let rhs_is_lvalue = matches!(
                                &**r,
                                Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
                            );
                            let rhs_is_ident = matches!(&**r, Expr::Ident { .. });
                            let rhs_is_compound = matches!(&**r, Expr::CompoundLit { .. });
                            self.push(0); // save target address
                            if rhs_is_lvalue {
                                // Lvalue RHS: get source address directly
                                self.gen_lvalue_addr(r, 0); // rax = source address
                                self.pop(1); // rcx = target address
                                self.emit(&[0x48, 0x89, 0xC6]); // mov rsi, rax
                            } else if rhs_is_ident {
                                // Ident RHS: struct variable address via LEA
                                if let Expr::Ident { name, .. } = &**r {
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
                            self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
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
                                    } else if matches!(
                                        fty,
                                        Type::Float | Type::Double | Type::LongDouble
                                    ) {
                                        self.emit_store_scalar_float_at(&r.ty(), fty, 1); // 单一真源：收浮影子副本
                                    } else {
                                        match size {
                                            1 => self.emit(&[0x88, 0x01]),
                                            2 => self.emit(&[0x66, 0x89, 0x01]),
                                            4 => self.emit(&[0x89, 0x01]),
                                            8 => self.emit(&[0x48, 0x89, 0x01]),
                                            16 => {
                                                // __int128: rax:rdx → [rcx]:[rcx+8]
                                                self.emit(&[0x48, 0x89, 0x01]); // mov [rcx], rax
                                                self.emit(&[0x48, 0x89, 0x51, 0x08]);
                                                // mov [rcx+8], rdx
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
                    let is_float_inner =
                        matches!(inner, Type::Float | Type::Double | Type::LongDouble);
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
                            // LongDouble 分量: gen_expr 后值在 st0 (80-bit
                            // tbyte), 必须 fstpt 直写 [rcx+part_off]。绝不
                            // 能走下方 SSE movsd — 只写 8 字节且取 xmm0
                            // (垃圾), 破坏 real/imag 的 10 字节 tbyte 槽。
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
                    let is_float_ty =
                        matches!(assign_l_ty, Type::Float | Type::Double | Type::LongDouble);
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
                            if matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble)
                            {
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
                            self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
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
            },
            Expr::Unary { op, e, .. } => {
                // 浮点字面量取负: 编译期折叠, 直接生成相反值的 Float 表达式
                if matches!(op, UnOp::Neg) {
                    if let Expr::Float { c, ty, span, .. } = e.as_ref() {
                        let negated = Expr::Float {
                            c: c.negate(),
                            ty: ty.clone(),
                            span: span.clone(),
                        };
                        self.gen_expr(&negated, dst);
                        return;
                    }
                }
                if matches!(e.ty(), Type::Int128 | Type::UInt128) {
                    self.gen_expr(e, 0);
                    match op {
                        UnOp::Neg => {
                            self.emit(&[0x48, 0xF7, 0xD8]); // neg rax
                            self.emit(&[0x48, 0x83, 0xD2, 0x00]); // adc rdx, 0
                            self.emit(&[0x48, 0xF7, 0xDA]); // neg rdx
                        },
                        UnOp::BitNot => {
                            self.emit(&[0x48, 0xF7, 0xD0]); // not rax
                            self.emit(&[0x48, 0xF7, 0xD2]); // not rdx
                        },
                        UnOp::Not => {
                            self.emit(&[0x48, 0x09, 0xD0]); // or rax, rdx
                            self.emit(&[0x48, 0x85, 0xC0]); // test rax, rax
                            self.emit(&[0x0F, 0x94, 0xC0]); // sete al
                            self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
                            self.emit(&[0x48, 0x31, 0xD2]); // high = 0
                        },
                        UnOp::Plus => {},
                    }
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                    return;
                }
                // 浮点取负: 翻转符号位 (XOR with sign mask)
                // 否则在 gen_expr 之后 emit neg r64 会错把 rax 取反, 而浮点结果在 xmm0.
                if matches!(op, UnOp::Neg)
                    && (matches!(e.ty(), Type::Double | Type::Float | Type::LongDouble)
                        || matches!(e.ty(), Type::Imaginary(_)))
                {
                    self.gen_expr(e, 0); // 值按类型落在 st(0) 或 xmm0
                    let e_ty = e.ty();
                    let neg_ty = if let Type::Imaginary(ref inner) = e_ty {
                        &inner.base
                    } else {
                        &e_ty
                    };
                    // 单一判据：st(0) 通道用 `fchs`（翻转 80-bit 的符号位，
                    // 不经 xmm0、不丢尾数）；否则才用 xorpd 翻符号位掩码。
                    // `Imaginary` 参内型的符号位存在内存对里，不适用 st(0)。
                    if self.float_in_st0(neg_ty) && !matches!(e_ty, Type::Imaginary(_)) {
                        self.emit_x87_chs();
                    } else {
                        match neg_ty {
                            Type::Float => {
                                // 翻转 32 位符号位: movabs rax, 0x8000_0000
                                //   66 48 0f 6e c8 movq xmm1, rax
                                //   0f 57 c1       xorps xmm0, xmm1
                                self.emit(&[0x48, 0xB8]);
                                self.emit(&0x80000000u64.to_le_bytes());
                                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]);
                                self.emit(&[0x0F, 0x57, 0xC1]);
                            },
                            _ => {
                                // 翻转 64 位符号位（double，以及 -mno-sse 下
                                // 担心遗漏的兼容分支已由上方 st(0) 接走）
                                self.emit(&[0x48, 0xB8]);
                                self.emit(&0x8000000000000000u64.to_le_bytes());
                                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]);
                                self.emit(&[0x66, 0x0F, 0x57, 0xC1]);
                            },
                        }
                    }
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                    return;
                }
                // _Complex 取负: 实部/虚部分别翻转符号位 (结果布局同
                // gen_complex_binop: 实部 xmm0, 分量对在 [r13+result_off])
                if matches!(op, UnOp::Neg) && matches!(e.ty(), Type::Complex(_)) {
                    let inner = match &e.ty() {
                        Type::Complex(i) => i.base.clone(),
                        _ => Type::Double,
                    };
                    let inner_sz = inner.size() as i64;
                    let result_off = -self.frame_size + 8;
                    // complex LD: 两分量各 fchs (值通道是 x87), 物化到帧槽,
                    // rax=result_off (LD rvalue 约定)。绝不能落到下方 SSE
                    // movsd/flip_sign 路径 — 那只读/写 8 字节, 会破坏 10 字节
                    // tbyte (real/imag 双双变 nan)。
                    if matches!(inner, Type::LongDouble) {
                        let src = self.gen_complex_ld_materialize(e);
                        self.emit_x87_fld_at(13, src as i32, &inner);
                        self.emit_x87_chs();
                        self.emit_x87_fstp_at(13, result_off as i32, &inner);
                        self.emit_x87_fld_at(13, src as i32 + 16, &inner);
                        self.emit_x87_chs();
                        self.emit_x87_fstp_at(13, (result_off + inner_sz) as i32, &inner);
                        self.emit_lea_r13(0, result_off);
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return;
                    }
                    let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                    // 1. 实部 → xmm0, 虚部 → [r13+result_off+inner_sz]
                    if self.gen_lvalue_addr(e, 0) {
                        // lvalue (变量/字段/下标): 用 rcx 保存地址 (flip_sign_xmm
                        // 会破坏 rax 但不动 rcx), 逐分量加载/flip/store
                        self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                        let imag_off = inner_sz; // 变量内存虚部槽 (连续布局)
                                                 // 实部: [rcx] → xmm0 → flip → [result_off]
                        self.emit(&[pfx, 0x0F, 0x10, 0x01]); // movss/movsd xmm0, [rcx]
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off);
                        // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                        self.emit(&[pfx, 0x0F, 0x10, 0x81]);
                        self.emit(&(imag_off as i32).to_le_bytes());
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off + inner_sz);
                    } else {
                        // rvalue: gen_expr 后 xmm0=实部
                        self.gen_expr(e, 0);
                        if matches!(e.as_ref(), Expr::Call { .. }) {
                            // SSE 返回: 虚部在 xmm1, 物化到临时区
                            self.emit(&[pfx, 0x41, 0x0F, 0x11]);
                            if (result_off + inner_sz).abs() < 128 {
                                self.emit(&[0x4D, (result_off + inner_sz) as u8]);
                            } else {
                                self.emit(&[0x8D]);
                                self.emit(&((result_off + inner_sz) as i32).to_le_bytes());
                            }
                        }
                        // 实部翻转符号位并写回临时区
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off);
                        // 虚部翻转符号位并写回临时区
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x10], result_off + inner_sz);
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off + inner_sz);
                    }
                    // 重载实部到 xmm0 (complex rvalue 约定: xmm0=实部)
                    self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x10], result_off);
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                    return;
                }
                // _Complex 取共轭 (~): 实部不变, 虚部翻转符号。
                // 浮点 inner: flip_sign 翻浮点符号位; 整数 inner: 整数取负
                // (旧实现统一 movsd+flip_sign, 整数 _Complex 得到
                // (0, 原虚部) — complex-6 test_int)。
                if matches!(op, UnOp::BitNot) && matches!(e.ty(), Type::Complex(_)) {
                    let inner = match &e.ty() {
                        Type::Complex(i) => i.base.clone(),
                        _ => Type::Double,
                    };
                    let inner_sz = inner.size() as i64;
                    let result_off = -self.frame_size + 8;
                    // complex LD 共轭: 源只求值一次物化成 tbyte 对, real 直写,
                    // imag fchs 后写 (值通道是 x87, 不能走下方 SSE 副本)。
                    if matches!(inner, Type::LongDouble) {
                        let src = self.gen_complex_ld_materialize(e);
                        self.emit_x87_fld_at(13, src as i32, &inner);
                        self.emit_x87_fstp_at(13, result_off as i32, &inner);
                        self.emit_x87_fld_at(13, src as i32 + 16, &inner);
                        self.emit_x87_chs();
                        self.emit_x87_fstp_at(13, (result_off + inner_sz) as i32, &inner);
                        self.emit_lea_r13(0, result_off);
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return;
                    }
                    if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
                        let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                        if self.gen_lvalue_addr(e, 0) {
                            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                            let imag_off = inner_sz;
                            // 实部: [rcx] → xmm0 → [result_off] (不变)
                            self.emit(&[pfx, 0x0F, 0x10, 0x01]); // movss/movsd xmm0, [rcx]
                            self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off);
                            // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                            self.emit(&[pfx, 0x0F, 0x10, 0x81]);
                            self.emit(&(imag_off as i32).to_le_bytes());
                            self.flip_sign_xmm(0, &inner);
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x49, 0x0F, 0x11],
                                result_off + inner_sz,
                            );
                        } else {
                            self.gen_expr(e, 0);
                            // 实部已经在 xmm0, 直接写回
                            self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x11], result_off);
                            // 虚部: 加载并翻转
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x49, 0x0F, 0x10],
                                result_off + inner_sz,
                            );
                            self.flip_sign_xmm(0, &inner);
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x49, 0x0F, 0x11],
                                result_off + inner_sz,
                            );
                        }
                        // 重载实部到 xmm0
                        self.emit_xmm0_r13_disp(&[pfx, 0x49, 0x0F, 0x10], result_off);
                        // rax = result_off (dcc complex rvalue 约定, 供
                        // spill_complex_operand 的 rep movsb / gen_return 读取)
                        self.emit_lea_r13(0, result_off);
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                        return;
                    }
                    // 整数 inner: 共轭 = 虚部整数取负。
                    // 1. 源物化到 result_off (保二进制布局)
                    if self.gen_lvalue_addr(e, 0) {
                        self.mov(6, 0); // rsi = &src
                        self.emit_lea_r13(7, result_off); // rdi = r13+result_off
                        self.mov_imm(1, (inner_sz * 2) as i128);
                        self.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
                    } else {
                        self.gen_expr(e, 0);
                        // rvalue 已按统一约定物化到 result_off (rax=地址);
                        // 保险起见若 rax 不指向 result_off 则重拷
                        // (整数 complex rvalue 均遵守此约定)
                    }
                    // 2. 虚部取负 (按内型宽度)
                    match inner_sz {
                        8 => {
                            self.load_stack(0, result_off + 8);
                            self.emit(&[0x48, 0xF7, 0xD8]); // neg rax
                            self.store_stack(0, result_off + 8);
                        },
                        4 => {
                            self.load_stack(0, result_off + 4);
                            self.emit(&[0xF7, 0xD8]); // neg eax
                            self.emit_store(0, result_off + 4, 4);
                        },
                        2 => {
                            self.load_stack(0, result_off + 2);
                            self.emit(&[0x66, 0xF7, 0xD8]); // neg ax
                            self.emit_store(0, result_off + 2, 2);
                        },
                        _ => {
                            self.load_stack(0, result_off + 1);
                            self.emit(&[0xF6, 0xD8]); // neg al
                            self.emit_store(0, result_off + 1, 1);
                        },
                    }
                    // 3. rax = result_off (统一 rvalue 约定: rax 指向
                    // 物化结果, 消费端从内存对读取)
                    self.emit_lea_r13(0, result_off);
                    if dst != 0 {
                        self.mov(dst, 0);
                    }
                    return;
                }
                // Vector 取负/按位非: 逐元素操作, 结果写入约定槽 0
                // (dcc vector rvalue 约定: 结果在 vector_temp_base(0),
                // 消费端 gen_aggregate_assign 等按此读取)
                if matches!(e.ty(), Type::Vector(_, _)) {
                    if let Type::Vector(ref elem_ty, count) = e.ty() {
                        self.gen_vector_unary_into_slot(*op, e, &elem_ty.base, count, 0);
                        return;
                    }
                }
                self.gen_expr(e, dst);
                match op {
                    UnOp::Neg => {
                        // neg r/m: 根据类型大小选择操作数尺寸。
                        // 64 位类型用 REX.W; 32 位有符号 + trapv 用 32 位 neg
                        // (检测 32 位溢出); 32 位有符号无 trapv 用 REX.W
                        // (寄存器保存符号扩展值, 64 位 neg 保持上 32 位一致);
                        // 32 位无符号用 REX (无 .W) 保持零扩展。
                        let ty_size = e.ty().size();
                        let is_signed = Self::is_signed_int(&e.ty());
                        let use_32bit_neg = ty_size == 4 && is_signed && self.trapv;
                        if ty_size >= 8 || (ty_size >= 4 && is_signed && !use_32bit_neg) {
                            // 64-bit neg: 对有符号 32+ 位类型 (非 trapv),
                            // 寄存器值已符号扩展到 64 位, 必须用
                            // REX.W neg 保持上 32 位一致
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]);
                        } else if use_32bit_neg {
                            // 32-bit neg for trapv: 检测 32 位溢出
                            let rex = 0x40 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]);
                        } else {
                            // 32 位无符号: REX 前缀无 .W
                            let rex = 0x40 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]);
                        }
                        // -ftrapv: 有符号取负溢出检查 (e.g. -INT_MIN)
                        // neg 在操作数为类型最小值时置 OF=1
                        if self.trapv && Self::is_signed_int(&e.ty()) {
                            let trap_label = self.fresh_label();
                            let ok = self.fresh_label();
                            self.jmp_rel32(&trap_label, JumpKind::Jo);
                            self.jmp_rel32(&ok, JumpKind::Jmp);
                            self.label(&trap_label);
                            self.emit(&[0xe8, 0, 0, 0, 0]); // call rel32
                            let call_at = self.code.len() as u64 - 4;
                            self.pending_func_rels
                                .push((call_at, "__dcc_trapv_overflow".to_string()));
                            self.label(&ok);
                            // 32 位 trapv neg 后符号扩展: 32 位 neg 结果在 eax,
                            // 需要 movslq 到 rax 保持 dcc 的 64 位寄存器约定
                            if e.ty().size() == 4 {
                                self.emit(&[0x48, 0x63, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            }
                        }
                    },
                    UnOp::Not => {
                        // 逻辑非: !x = (x == 0) ? 1 : 0
                        // test dst, dst
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        // sete al: 零 → 1, 非零 → 0 (0F 94 C0)
                        self.emit(&[0x0F, 0x94, 0xC0]);
                        // movzx dst, al (REX 0F B6 modrm)
                        // ModRM: mod=11, reg=dst, r/m=0(al) → 0xC0 | ((dst&7)<<3)
                        // REX.R (0x04) when dst>=8 (reg field extension)
                        let rex2 = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                        self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3)]);
                    },
                    UnOp::BitNot => {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0xf7, 0xd0 | (dst & 7)]);
                        // not flips all 64 bits. The result must be
                        // truncated to the expression type width, then
                        // re-extended. For types < int, C integer
                        // promotion means the NOT result is at least
                        // 32 bits wide.
                        let result_ty = e.ty();
                        let result_sz = result_ty.size();
                        let effective_sz = result_sz.max(4); // int promotion
                        if effective_sz <= 4 {
                            // Result fits in 32 bits: truncate to 32
                            // bits then zero/sign-extend to 64.
                            let is_signed =
                                Self::is_signed_int(&result_ty) || matches!(result_ty, Type::Char);
                            if is_signed {
                                // movsxd dst, dst (sign-extend 32→64)
                                let rex = 0x48 | if dst >= 8 { 0x04 } else { 0 };
                                self.emit(&[rex, 0x63, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            } else {
                                // mov dst32, dst32 (zero-extend 32→64)
                                let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
                                self.emit(&[rex, 0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            }
                        } else if result_sz < 8 {
                            // 8-byte types: no fixup needed (not already
                            // produces correct 64-bit result)
                            unreachable!()
                        }
                    },
                    UnOp::Plus => {},
                }
            },
            Expr::Call {
                callee, args, ty, ..
            } => {
                // 单一入口：本 arm 曾是 `gen_expr_call` 的逐字副本。
                self.gen_expr_call(callee, args, ty, dst);
            },
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
            Expr::Comma { l, r, .. } => {
                // 逗号运算符: 求值 lhs 丢弃, 求值 rhs 结果放入 dst
                self.gen_expr(l, 0);
                self.gen_expr(r, dst);
            },
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ..
            } => {
                self.current_asm_src_line = span.line;
                self.emit_inline_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    *volatile,
                );
                if dst != 0 {
                    self.mov(dst, 0);
                }
            },
            Expr::LabelAddr { label, .. } => {
                // &&label: 加载标签地址到 dst (GNU 扩展)
                // 使用 LEA dst, [RIP + disp32] + label_addr_refs 延迟解析
                // 前缀标签名以避免与函数符号名冲突
                let label_name = Self::c_label_to_asm(&label.name);
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                let modrm = 0x05 | ((dst & 7) << 3);
                self.emit(&[rex, 0x8D, modrm, 0, 0, 0, 0]);
                let disp32_pos = self.pos() - 4;
                self.label_addr_refs.push((disp32_pos, label_name));
            },
            Expr::StmtExpr { block, .. } => {
                // 语句表达式 ({...}): 执行块内语句, 最后表达式值留在 dst
                let items = &block.items;
                let last_idx = items.len().saturating_sub(1);
                for (i, item) in items.iter().enumerate() {
                    match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) if i == last_idx => {
                            // 最后一条表达式语句: 结果放入 dst
                            let e_ty = e.ty();
                            // 聚合类型 (struct/union) 的 StmtExpr 最后一条处理:
                            // - lvalue (Ident/Member/Index/Deref): 取地址 + 整体拷贝到 sret 临时槽
                            //   (不能 gen_expr 加载前 8 字节值, 调用方期望地址:
                            //    pr c/5354: ({ struct S s; s.a=1; s; }).a)
                            // - Call: SysV ABI >16B 返回已走 sret 约定 (rax=buffer 地址),
                            //   否则物化寄存器返回值到临时槽
                            // - 标量/其他: 正常 gen_expr
                            if matches!(e_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
                                let sz = e_ty.size();
                                if self.gen_lvalue_addr(e, 6) {
                                    // lvalue 路径: 整体拷贝到 sret 临时槽
                                    // rsi 已 = 源地址 (gen_lvalue_addr 放到 dst=6 即 rsi)
                                    let temp_off = self.alloc_sret_slot(sz);
                                    self.emit_lea_r13(7, temp_off); // rdi = sret 临时槽
                                    self.mov_imm(1, sz as i128); // rcx = size
                                    self.emit(&[0xFC]); // cld
                                    self.emit(&[0xF3, 0xA4]); // rep movsb
                                    self.emit_lea_r13(dst, temp_off);
                                } else if Self::needs_memory_return(&e_ty) {
                                    // Call + >16B: SysV ABI sret, rax 已是返回地址
                                    self.gen_expr(e, dst);
                                } else if matches!(e, Expr::Call { .. }) {
                                    // Call + <=16B: 寄存器返回, 物化到临时槽
                                    self.gen_expr(e, 0);
                                    let temp_off = self.alloc_sret_slot(sz);
                                    if sz <= 8 {
                                        self.store_stack(0, temp_off);
                                    } else {
                                        self.store_stack(0, temp_off);
                                        self.store_stack(2, temp_off + 8);
                                    }
                                    self.emit_lea_r13(dst, temp_off);
                                } else {
                                    // 其他非 lvalue 聚合 (CompoundLit 等): 让 gen_expr 自己处理
                                    self.gen_expr(e, dst);
                                }
                            } else {
                                self.gen_expr(e, dst);
                            }
                        },
                        BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) if i == last_idx => {
                            self.gen_expr(e, dst);
                        },
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            // static 局部变量已在全局数据段分配
                            if v.is_static
                                && self
                                    .static_local_map
                                    .is_static_local(&self.current_func_name, &v.name.name)
                                && !self
                                    .static_local_map
                                    .needs_runtime_init(&self.current_func_name, &v.name.name)
                            {
                                continue;
                            }
                            // static local 需要运行时初始化
                            if v.is_static
                                && self
                                    .static_local_map
                                    .needs_runtime_init(&self.current_func_name, &v.name.name)
                            {
                                if let Some(init) = &v.init {
                                    self.gen_expr(init, 0);
                                    self.emit_global_store(0, &v.name.name);
                                }
                                continue;
                            }
                            self.register_var(&v.name.name, &v.ty);
                            // 定义+声明一体的 struct/union 类型绑定 +
                            // VLA/VM struct 变量动态分配 (语句表达式内)
                            if matches!(v.ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
                                self.gen_vm_type_bind(&v.ty);
                            }
                            // VLA/VM struct 变量: 语句表达式内也需动态分配
                            // 数据区并写元数据槽 (20070919-1: ({ struct S b; ... }))
                            if Self::type_has_vla(&v.ty) {
                                self.gen_vla_alloc(v);
                            }
                            if let Some(init) = &v.init {
                                // 浮点: 结果在 xmm0, 必须用 movsd/movss 存回栈 (同 Stmt::Decl 路径)
                                if matches!(&v.ty, Type::Float | Type::Double | Type::LongDouble) {
                                    self.gen_expr(init, 0);
                                    self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                                    self.gen_float_store(&v.name.name, &v.ty);
                                } else {
                                    self.gen_expr(init, 0);
                                    // Implicit float→int conversion when
                                    // initializer is float but variable is
                                    // integer (e.g. `int a = 0.0;`).
                                    let init_ty = init.ty();
                                    if matches!(
                                        &init_ty,
                                        Type::Float | Type::Double | Type::LongDouble
                                    ) && Self::is_integer_type(&v.ty)
                                    {
                                        self.emit_implicit_float_to_int(0, &init_ty, &v.ty);
                                    }
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            }
                        },
                        BlockItem::Decl(Decl::StructDef(sd)) => {
                            // 局部类型声明: 重新绑定 VM struct 大小槽
                            // (与 gen_block_items 同一语义)
                            self.gen_vm_struct_bind(sd, false);
                        },
                        BlockItem::Decl(Decl::UnionDef(sd)) => {
                            self.gen_vm_struct_bind(sd, true);
                        },
                        _ => {},
                    }
                }
            },
            Expr::Cast { ty, e, .. } => {
                // 单一入口：本 arm 曾是 `gen_expr_cast` 的逐字副本。此处模式
                // 内层 `e` 遮蔽了 gen_expr 的形参 `e`, 而 gen_expr_cast 要的
                // 正是那个被转换的子表达式, 语义一致。
                self.gen_expr_cast(ty, e, dst);
            },
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                self.gen_expr(cond, 0);
                self.emit_truth_flags(&cond.ty());
                let le = self.fresh_label();
                let lend = self.fresh_label();
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_expr(then_, dst);
                self.jmp_rel32(&lend, JumpKind::Jmp);
                self.label(&le);
                self.gen_expr(else_, dst);
                self.label(&lend);
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
            Expr::Float { c, ty, .. } => {
                // 浮点字面量: 存入 .rodata, 用 movsd/movss 加载到 xmm0
                self.gen_float_imm_load(*c, ty);
            },
            Expr::ComplexLit { .. } | Expr::ComplexIntLit { .. } => {
                // 复数字面量统一走 compound.rs 单一实现（含整型精确 i128
                // 分量），避免两处维护同一发射逻辑。
                self.gen_expr_complex_lit(e, dst);
            },
            Expr::Real { e, ty, .. } => {
                // __real__ expr: 取 _Complex 表达式实部; 标量返回自身
                self.gen_complex_part(e, true, ty, dst);
            },
            Expr::Imag { e, ty, .. } => {
                // __imag__ expr: 取 _Complex 表达式虚部; 标量返回 0
                self.gen_complex_part(e, false, ty, dst);
            },
            Expr::Index {
                arr,
                idx,
                ty: elem_ty,
                span,
                ..
            } => {
                // 单一入口：见 Member arm 同款说明 (影子副本收敛)。
                self.gen_expr_index_rvalue(e, arr, idx, elem_ty, span, dst);
            },
            Expr::PostInc { e, .. } => self.gen_inc_dec(e, dst, false, true),
            Expr::PostDec { e, .. } => self.gen_inc_dec(e, dst, false, false),
            Expr::PreInc { e, .. } => self.gen_inc_dec(e, dst, true, true),
            Expr::PreDec { e, .. } => self.gen_inc_dec(e, dst, true, false),
            Expr::Deref { e, ty, .. } => {
                // 单一入口：见 Member arm 同款说明 (影子副本收敛)。
                self.gen_expr_deref_rvalue(e, ty, dst);
            },
            Expr::AddrOf { e, .. } => {
                // &var / &struct.field / &ptr->field / *ptr: 计算地址到 dst
                if let Expr::Ident {
                    name, ty: id_ty, ..
                } = e.as_ref()
                {
                    if matches!(id_ty, Type::Func(_, _)) && !self.var_map.contains_key(&name.name) {
                        // &func: 函数地址
                        // 沿作用域链解析嵌套函数引用 (支持兄弟/祖先定义)
                        if let Some((unique, hops, offs)) = self.resolve_nested_func_ref(&name.name)
                        {
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
                        let off = self.var_offset(&name.name);
                        // REX.W + REX.B (r13) + REX.R (当 dst >= 8, 编码 r10/r11/r12/...)
                        let rex = 0x48u8 | 0x01 | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[rex, 0x8D]);
                        if off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&(off as i32).to_le_bytes());
                        }
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
                            let off = self.var_offset(&name.name);
                            let rex = 0x49u8;
                            self.emit(&[rex, 0x8D]);
                            if off.abs() < 128 {
                                self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                            } else {
                                self.emit(&[0x85 | ((dst & 7) << 3)]);
                                self.emit(&(off as i32).to_le_bytes());
                            }
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
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                            self.emit(&field_off.to_le_bytes());
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
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
                        } else if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                            // 局部数组: LEA 取地址
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        } else {
                            // 局部指针: 加载指针值
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
                        }
                    } else {
                        // 通用路径: 基址来自任意 lvalue/指针表达式 (如 &s.i[k])
                        let arr_ty = arr.ty();
                        if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                            if !self.gen_lvalue_addr(arr, 0) {
                                self.gen_expr(arr, 0);
                            }
                        } else {
                            self.gen_expr(arr, 0);
                        }
                    }
                    self.push(0);
                    self.gen_expr(idx, 1);
                    self.pop(0);
                    self.emit_index_scale(es, arr, elem_ty);
                    self.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx
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
                            let off = self.var_offset(&name.name);
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
                            let off = self.var_offset(&name.name);
                            self.emit_lea_r13(dst, off);
                        }
                    } else if !self.gen_lvalue_addr(inner, dst) {
                        self.gen_expr(inner, dst);
                    }
                    // 加上虚部偏移
                    if inner_sz != 0 {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                        self.emit(&(inner_sz as i32).to_le_bytes());
                    }
                }
            },
            Expr::Member {
                expr,
                field,
                is_arrow,
                ty,
                ..
            } => {
                // 单一入口：本 arm 曾是 `gen_expr_member_rvalue` 的逐字副本
                // (两份同源代码, 只有这一份在跑), 任何只改 lvalue.rs 的修复
                // 都会静默失效。现只委派。
                self.gen_expr_member_rvalue(expr, field, is_arrow, ty, dst);
            },
            Expr::CompoundLit { ty, inits, .. } => {
                if let Type::Vector(elem_ty, n) = ty {
                    self.gen_vector_init_rvalue(inits, &elem_ty.base, *n, 0);
                    let vec_size = elem_ty.base.size() * n;
                    let temp_off = self.vector_temp_base(0, vec_size);
                    self.emit_lea_r13(dst, temp_off);
                    return;
                }
                // 复合字面量: (type){...} - 在栈上分配可变内存, 初始化后返回地址
                // 用唯一名称注册到 var_map, 复用 register_var 的栈分配逻辑
                let compound_name = format!("__compound_lit_{}", self.pos());
                let base_offset = self.register_var(&compound_name, ty);
                // 生成初始化代码
                self.gen_init_list_at(base_offset, ty, inits);
                // dst = r13 + base_offset (compound lit address)
                self.emit_lea_r13(dst, base_offset);
            },
            Expr::VaArg { .. } => {
                // 单一入口：本 arm 曾是 `gen_expr_va_arg` 的逐字副本（两份同源
                // 代码，只有这一份在跑），任何只改 va_arg.rs 的修复都会静默
                // 失效。现只委派，交付约定与地址解析判据全部收在
                // emit/expr/va_arg.rs。
                self.gen_expr_va_arg(e, dst);
            },
            Expr::InitList { inits, .. } => {
                if let Type::Vector(elem_ty, n) = e.ty() {
                    self.gen_vector_init_rvalue(inits, &elem_ty.base, n, 0);
                    // rax 指向临时槽, 供后续 aggregate_assign 使用
                    let vec_size = elem_ty.base.size() * n;
                    let temp_off = self.vector_temp_base(0, vec_size);
                    self.emit_lea_r13(0, temp_off);
                } else if e.ty().is_arith() || e.ty().is_ptr() || matches!(e.ty(), Type::Bool) {
                    // 标量初始化列表 (如 `int x = {1};` 或带 brace 的复合字面量):
                    // 第一个元素就是要赋的值, 后续 (C99 6.7.8/11) 只能是 0
                    if let Some(first) = inits.first() {
                        let v = match first {
                            InitItem::Expr(ex) => ex,
                            InitItem::Designated { value, .. } => value,
                        };
                        self.gen_expr(v, dst);
                    } else {
                        self.mov_imm(dst, 0);
                    }
                } else {
                    self.expr_warn(
                        e,
                        &format!(
                            "codegen unsupported InitList rvalue for {:?}; emitted as ud2",
                            e.ty()
                        ),
                    );
                    self.emit(&[0x0F, 0x0B]); // ud2
                    self.mov_imm(dst, 0);
                }
            },
            _ => {
                self.expr_warn(e, "codegen unsupported expression; emitted as ud2");
                self.emit(&[0x0F, 0x0B]); // ud2
                self.mov_imm(dst, 0);
            },
        }
    }
}
