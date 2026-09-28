// src/codegen/x86_64/emit/stmt.rs
// Statement code generation for x86_64 backend.
// 本文件由 mod.rs 逐字搬迁而来 (batch 2): `gen_stmt` 原样剪切至 `impl Codegen`,
// 语义 diff 为 0。

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::{AbiClass, AggDst, Codegen, JumpKind};

use crate::const_eval;

impl Codegen {
    pub fn gen_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    // 数组表达式在返回值上下文中衰减为指针
                    // (int *f() { int a[4]; return a; } → 返回 &a)。
                    // 若不衰减, 12B+ 数组会误走聚合按值返回路径 (读内容)。
                    let ret_ty = match e.ty() {
                        Type::Array(elem, _) => Type::Ptr(elem),
                        t => t,
                    };
                    let ret_sz = ret_ty.size();
                    // A1: 内存返回 (sret) — 聚合/长双精度整体写入 rdi 指向的缓冲区.
                    // 函数声明返回 long double 时, 即使表达式类型是 int/double
                    // (隐式转换), 也必须走 sret 写回 (调用方从 sret 槽读值).
                    let func_ret_is_ld = matches!(self.current_func_ret_ty, Type::LongDouble);
                    if func_ret_is_ld && self.current_sret_offset.is_none() {
                        // D10-3（已简化）：SysV 规定标量 `long double` 返回值在 **st(0)**。
                        // 本后端 `long double` 的 rvalue 值通道已经就是 st(0)，所以
                        // 返回表达式本身就是 `fldt`/x87 运算的终点，**不需要任何中转**。
                        // 只有当被返回的表达式类型不是 st(0) 通道（整型 / f32 / f64）
                        // 时才需要一次精确拓宽：整数走 `fild`（80-bit 有 64 位显式
                        // 尾数，任意 int64/u64 无损），f32/f64 走内存中转 `fld`（同样是
                        // 子集拓宽，不引入舍入）。旧实现无条件 spill xmm0→`fld qword`，
                        // 那是在适配“ld 内部只有 f64 精度”的旧不变量，会把真 80-bit
                        // 返回值重新舍回 53 位尾数。
                        let e_ty = e.ty();
                        self.gen_expr(e, 0);
                        self.emit_operand_to_st0(&e_ty, 0);
                    } else if matches!(self.current_func_ret_ty, Type::Complex(_))
                        && self.current_sret_offset.is_none()
                    {
                        // 复数返回的**交付与隐式转换都按声明类型决定**: `ret_ty`
                        // 只是表达式类型, `cd f(int n){ return n; }` 的它是 int ——
                        // 若按表达式判分就会把 int 的位模式当复数对交回 (实测
                        // 分量全 0/垃圾), 且遗漏 C6.3.1.8p1 的标量→复数与
                        // p4 的异内型逐分量转换。两者都住在 `gen_complex_return`
                        // 里 (与形参侧共用 `complex_src_needs_convert` 单一真源)。
                        let cx_ty = self.current_func_ret_ty.clone();
                        self.gen_complex_return(e, &cx_ty);
                    } else if (Self::needs_memory_return(&ret_ty)
                        || func_ret_is_ld
                        || Self::needs_memory_return(&self.current_func_ret_ty))
                        && self.current_sret_offset.is_some()
                    {
                        // 同样按**声明**返回类型判分: 整数内型 `_Complex` 被
                        // `leaf_abi_class` force 成 Memory 类, 而 `cd`/`cf` 的表达式
                        // 类型可能是标量 (`ci f(int n){return n;}` 的 ret_ty 是 int),
                        // 只看表达式会整条 sret 臂落空。缓冲区尺寸/转换在
                        // `gen_sret_return` 内按声明类型处理。
                        self.gen_sret_return(e, &ret_ty);
                    } else {
                        // _Complex float (8B) 同样走 SSE 寄存器返回
                        // (xmm0=实部, xmm1=虚部), 不适用 >8B 门槛;
                        // 否则落入 gen_expr 整数路径, xmm0/xmm1 为垃圾
                        let is_agg_ret = if matches!(&ret_ty, Type::Complex(_)) {
                            true
                        } else {
                            matches!(
                                &ret_ty,
                                Type::Struct(_, _, _, _)
                                    | Type::Union(_, _, _, _)
                                    | Type::Array(_, ArraySize::Const(_))
                                    | Type::Atomic(_)
                                    | Type::Vector(_, _)
                            ) && ret_sz > 8
                        };
                        if is_agg_ret {
                            if let Type::Complex(_) = &ret_ty {
                                // 交付与隐式转换一并交给复数返回总入口
                                // (`gen_complex_return`): 返回上下文的标量→复数 /
                                // 异内型复数→复数转换 (C6.3.1.8) 必须在此发生,
                                // 否则 callee 把源的位模式当成目标分量交付。
                                self.gen_complex_return(e, &ret_ty);
                            } else {
                                // 16B [Sse, SseUp] 聚合返回值 (独立向量或含
                                // 16B 向量字段/对齐填充的小结构体): SysV ABI
                                // 全在 xmm0, 不走 rax/rdx → xmm0/xmm1 拆分.
                                // 直接 movups 16B 到 xmm0.
                                if Self::is_sseup_128_aggregate(&ret_ty) {
                                    // 动作只有一个: 把返回表达式的 16 字节**对象表示**
                                    // 整体装进 xmm0。先物化到一个帧内缓冲, 再一条
                                    // `movups` 读回 —— 物化契约 (Ident/Member/Index/
                                    // Deref 取地址、Call 走 ABI 返值、Vector 走
                                    // `vector_temp_base` 约定) 全部复用聚合赋值的同
                                    // 一真源 `gen_aggregate_assign_to`, 本站点不得重
                                    // 写任何形态判据或帧偏移。
                                    //
                                    // 旧写法在这里硬编码 `-frame_size + 8` 当作“向量
                                    // 临时槽”, 并从该处 movups。那只是一个巧合: 非变参
                                    // 函数的 `vec_temp_off` 历史上恰好等于 `-frame_size
                                    // + 8`。该巧合如今被两处正规改动打破 (帧规划为复数
                                    // rvalue 划出 `CX_FRAME_BAND` 固定带、变参函数再另加
                                    // 176 字节 save area), 硬编码因此变成读复数物化带的
                                    // 残值 —— `return (v4f){...}` / `return s;` 全栈垃圾。
                                    // 而 `-frame_size + 8` 的**真**契约是复数内存对, 更不
                                    // 能说向量临时槽。
                                    let buf_off = self.alloc_sret_slot(ret_sz.max(16));
                                    let dst = AggDst::Local {
                                        off: buf_off,
                                        vla: None,
                                    };
                                    self.gen_aggregate_assign_to(&dst, &ret_ty, e);
                                    // movups xmm0, [r13 + buf_off]
                                    self.load_xmm128_stack(0, buf_off);
                                } else {
                                    // struct 返回: 加载到 rax[:rdx]
                                    if let Expr::Ident { name, .. } = &e {
                                        if self.is_global_not_local(&name.name) {
                                            // 全局结构体变量: 用 RIP-relative 寻址加载
                                            self.emit_global_addr(0, &name.name); // rax = 全局变量地址
                                            if ret_sz <= 8 {
                                                match ret_sz {
                                                    8 => self.emit(&[0x48, 0x8B, 0x00]), // mov rax, [rax]
                                                    4 => {
                                                        self.emit(&[0x8B, 0x00]);
                                                        self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
                                                    }, // movzx
                                                    2 => {
                                                        self.emit(&[0x66, 0x8B, 0x00]);
                                                        self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
                                                    },
                                                    1 => self.emit(&[0x0F, 0xB6, 0x00]), // movzx al, [rax]
                                                    _ => {},
                                                }
                                            } else {
                                                // >8 字节: 先用地址取高 8 字节, 再取低 8 字节
                                                self.emit(&[0x48, 0x8B, 0x50, 0x08]); // mov rdx, [rax+8]
                                                self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                                            }
                                        } else {
                                            let off = self.var_offset(&name.name);
                                            self.load_stack(0, off); // rax = 前 8 字节
                                            if ret_sz > 8 {
                                                self.load_stack(2, off + 8); // rdx = 后 8 字节
                                            }
                                        }
                                    } else {
                                        // CompoundLit/InitList 等: gen_expr 返回地址 (CompoundLit)
                                        // 或垃圾值 (InitList). 需要物化后加载到 rax/rdx.
                                        if matches!(
                                            e,
                                            Expr::CompoundLit { .. } | Expr::InitList { .. }
                                        ) {
                                            // 使用帧底部临时区 (-frame_size+8)
                                            let temp_off = -self.frame_size + 8;
                                            let inits = match e {
                                                Expr::CompoundLit { inits, .. } => Some(inits),
                                                Expr::InitList { inits, .. } => Some(inits),
                                                _ => None,
                                            };
                                            if let Some(inits) = inits {
                                                self.gen_init_list_at(temp_off, &ret_ty, inits);
                                            }
                                            self.load_stack(0, temp_off); // rax = 前 8 字节
                                            if ret_sz > 8 {
                                                self.load_stack(2, temp_off + 8);
                                                // rdx = 后 8 字节
                                            }
                                        } else {
                                            // D4: 聚合 rvalue 以「rax = 缓冲区地址」交付
                                            // (`va_arg` 聚合走 `gen_expr_va_arg` 的
                                            // `alloc_sret_slot` 路径)。本分支的交付目标
                                            // 是 SysV **寄存器**返回（[Integer,..] →
                                            // rax[:rdx]，下方再把 Sse 类 movq 进 xmm0/xmm1），
                                            // 必须按内容装载；不装载则调用方把地址字节当
                                            // 数据用（实测 `return va_arg(ap, struct Pair64)`
                                            // 的 `p.a` = 临时槽地址、`p.b` = 残留 rdx）。
                                            let rax_is_agg_addr = match e {
                                                Expr::VaArg { ref ty, .. } => {
                                                    Self::va_arg_result_in_memory(ty)
                                                },
                                                _ => false,
                                            };
                                            self.gen_expr(e, 0);
                                            if rax_is_agg_addr {
                                                // 先取高 8 再取低 8: 后一条会覆写 rax
                                                self.emit(&[0x48, 0x8B, 0x50, 0x08]); // mov rdx, [rax+8]
                                                self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                                            }
                                        }
                                    }
                                    // SysV: SSE/SSEUP 类 eightbyte 返回在 xmm0/xmm1 (而非 rax/rdx),
                                    // 如 struct {double,double}. _Complex float/double 已由
                                    // gen_complex_sse_return 直接填充返回寄存器。
                                    let ret_classes = Self::abi_classes(&ret_ty);
                                    for (c, cls) in ret_classes.iter().enumerate() {
                                        if matches!(cls, AbiClass::Sse | AbiClass::SseUp) {
                                            let src = if c == 0 { 0u8 } else { 2u8 };
                                            self.emit_movq_gp_to_xmm(c as u8, src);
                                        }
                                    }
                                } // end else (non-Vector agg return)
                            }
                        } else {
                            self.gen_expr(e, 0);
                            // Small vector (<=8B) return: gen_expr puts the
                            // vector data in temp slot 0 on the stack.  SysV
                            // ABI classifies <=16B vectors as SSE, so the
                            // return value must be in xmm0 (xmm1 for the
                            // upper 8B of 16B vectors, handled by is_agg_ret
                            // path above).  Load from temp slot 0 into xmm0.
                            if matches!(&ret_ty, Type::Vector(_, _)) && ret_sz <= 8 {
                                let toff = self.vector_temp_base(0, ret_sz);
                                // vector < 8B 不能读穿自身槽位。
                                // 按真实字节宽度加载，再把零扩展位型移入 xmm0。
                                self.load_stack_xmm(0, toff, ret_sz, false);
                            }
                            // __int128 返回值: 若函数返回 __int128 但表达式非 __int128, rdx 未设置
                            let func_ret_ty_for_128 = self.current_func_ret_ty.clone();
                            if matches!(&func_ret_ty_for_128, Type::Int128 | Type::UInt128)
                                && !matches!(e.ty(), Type::Int128 | Type::UInt128)
                            {
                                if matches!(func_ret_ty_for_128, Type::UInt128)
                                    || matches!(
                                        e.ty(),
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
                                    self.emit(&[0x48, 0x99]); // cqo
                                }
                            }
                            // Small struct (<=8B) returned as value in rax.
                            // CompoundLit/InitList gen_expr returns an ADDRESS
                            // in rax; we must load the VALUE from that address.
                            if matches!(&ret_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
                                && ret_sz <= 8
                                && matches!(e, Expr::CompoundLit { .. } | Expr::InitList { .. })
                            {
                                match ret_sz {
                                    1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00]), // movzbq (%rax),%rax
                                    2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00]), // movzwq (%rax),%rax
                                    4 => self.emit(&[0x8B, 0x00]),             // mov (%rax),%eax
                                    8 => self.emit(&[0x48, 0x8B, 0x00]),       // mov (%rax),%rax
                                    _ => {},
                                }
                            }
                            // Integer → float return conversion:
                            // `return int_expr` in a function returning
                            // float/double. The integer result is in rax;
                            // convert to xmm0.
                            let expr_ty = e.ty();
                            let func_ret_ty = self.current_func_ret_ty.clone();
                            if matches!(&func_ret_ty, Type::Float | Type::Double | Type::LongDouble)
                                && Self::is_integer_type(&expr_ty)
                            {
                                self.emit_scalar_to_xmm0_if_needed(e, &func_ret_ty);
                            }
                            // Float/double return type mismatch:
                            // convert expression type to the declared
                            // return type.  E.g. `return float_var`
                            // in a function returning double needs
                            // cvtss2sd.
                            if matches!(&expr_ty, Type::Float)
                                && matches!(&func_ret_ty, Type::Double | Type::LongDouble)
                            {
                                // cvtss2sd xmm0, xmm0
                                self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]);
                            } else if matches!(&expr_ty, Type::Double | Type::LongDouble)
                                && matches!(&func_ret_ty, Type::Float)
                            {
                                // cvtsd2ss xmm0, xmm0
                                self.emit(&[0xF2, 0x0F, 0x5A, 0xC0]);
                            }
                            // Float/double -> integer return:
                            // convert xmm0 to integer in rax.
                            //
                            // 直接复用 `emit_implicit_float_to_int` 这一单一真源:
                            // 旧代码在这里又手写了一遍 cvtt*/cvttsd2si 编码, 并把
                            // `long double` 当作「已在 xmm0」—— 而它的值通道是 x87
                            // st(0), 于是 `int f(void){ return ld; }` 读到陈旧 xmm0
                            // 得 0 (与隐式截断同一根因)。
                            if matches!(&expr_ty, Type::Float | Type::Double | Type::LongDouble)
                                && func_ret_ty.is_int()
                            {
                                self.emit_implicit_float_to_int(0, &expr_ty, &func_ret_ty);
                            }
                            // Truncate return value to the function's
                            // declared return type width. For types smaller
                            // than int, the BinOp result may have extra bits
                            // in the register that must be cleared.
                            let ret_bits = func_ret_ty.size() * 8;
                            if func_ret_ty.is_int() && ret_bits < 64 {
                                let is_signed = Self::is_signed_int(&func_ret_ty);
                                match ret_bits {
                                    8 if is_signed => {
                                        // movsbq %al, %rax (sign-extend to 64-bit)
                                        self.emit(&[0x48, 0x0F, 0xBE, 0xC0]);
                                    },
                                    8 => {
                                        // movzbq %al, %rax (zero-extend to 64-bit)
                                        self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
                                    },
                                    16 if is_signed => {
                                        // movswq %ax, %rax (sign-extend to 64-bit)
                                        self.emit(&[0x48, 0x0F, 0xBF, 0xC0]);
                                    },
                                    16 => {
                                        // movzwq %ax, %rax (zero-extend to 64-bit)
                                        self.emit(&[0x48, 0x0F, 0xB7, 0xC0]);
                                    },
                                    32 if is_signed => {
                                        // movsxd %eax, %rax (sign-extend to 64-bit)
                                        self.emit(&[0x48, 0x63, 0xC0]);
                                    },
                                    32 => {
                                        // mov %eax, %eax (zero-extend to 64-bit)
                                        self.emit(&[0x89, 0xC0]);
                                    },
                                    _ => {},
                                }
                            }
                        }
                    }
                    // __attribute__((returns_nonnull)): 返回前检查 rax 非空.
                    // 若为 0 触发 UD2 (执行未定义指令) 立即 abort.
                    if self.current_returns_nonnull
                        && matches!(ret_ty, Type::Ptr(_) | Type::Func(_, _))
                    {
                        // test rax, rax; jnz .ok; ud2; .ok:
                        self.emit(&[0x48, 0x85, 0xc0]); // test rax, rax
                        let ok_label = self.fresh_label();
                        self.jmp_rel32(&ok_label, JumpKind::Jne);
                        self.emit(&[0x0f, 0x0b]); // ud2 - undefined instruction trap
                        self.label(&ok_label);
                    }
                } else {
                    self.emit(&[0x31, 0xc0]);
                }
                // return 语句: 在 epilogue 前先插入函数退出插桩
                if self.target.instrument_functions && !self.current_no_instrument {
                    self.emit_instrument_func_exit();
                }
                self.emit_func_epilogue();
            },
            Stmt::Expr { e, .. } => {
                if !self.gen_unknown_builtin_stmt(e) {
                    self.gen_expr(e, 0);
                }
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                // 常量条件优化: if (1) 只生成 then, if (0) 只生成 else
                // 这对 __compiletime_assert 等内核宏至关重要:
                // 条件为真时不应生成对 __compiletime_assert_N 的调用
                //
                // 注意: 不能简单跳过死分支, 因为其中可能包含 goto 目标标签.
                // 如果死分支含有标签, 必须正常生成该分支代码.
                if let Ok(cv) = const_eval::eval(cond) {
                    let has_labels_in_then = Self::stmt_contains_label(then_);
                    if cv != 0 {
                        // condition is true
                        self.gen_stmt(then_);
                    } else if has_labels_in_then {
                        // condition is false but then-body has labels that
                        // external gotos may target — must generate it
                        self.gen_stmt(then_);
                    } else if let Some(el) = else_ {
                        // condition is false, no labels in then-body
                        self.gen_stmt(el);
                    }
                    // else: condition is false, no labels, no else → skip
                } else {
                    self.gen_expr(cond, 0);
                    self.emit_truth_flags(&cond.ty());
                    let le = self.fresh_label();
                    let lend = self.fresh_label();
                    self.jmp_rel32(&le, JumpKind::Je);
                    self.gen_stmt(then_);
                    // Skip dead jmp when then-body ends with return
                    let then_ends_ret = Self::stmt_ends_with_return(then_);
                    if !then_ends_ret {
                        self.jmp_rel32(&lend, JumpKind::Jmp);
                    }
                    self.label(&le);
                    if let Some(el) = else_ {
                        self.gen_stmt(el);
                    }
                    self.label(&lend);
                }
            },
            Stmt::While { cond, body, .. } => {
                let ls = self.fresh_label();
                let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone());
                self.loop_break = Some(le.clone());
                self.break_targets.push((le.clone(), false));
                self.maybe_align_loop();
                self.label(&ls);
                self.gen_expr(cond, 0);
                self.emit_truth_flags(&cond.ty());
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_stmt(body);
                self.jmp_rel32(&ls, JumpKind::Jmp);
                self.label(&le);
                self.loop_continue = sc;
                self.loop_break = sb;
                self.break_targets.pop();
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.gen_stmt(i);
                }
                let ls = self.fresh_label();
                let le = self.fresh_label();
                let lst = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(lst.clone());
                self.loop_break = Some(le.clone());
                self.break_targets.push((le.clone(), false));
                self.maybe_align_loop();
                self.label(&ls);
                if let Some(c) = cond {
                    self.gen_expr(c, 0);
                    self.emit_truth_flags(&c.ty());
                    self.jmp_rel32(&le, JumpKind::Je);
                }
                self.gen_stmt(body);
                self.label(&lst);
                if let Some(s) = step {
                    self.gen_expr(s, 0);
                }
                self.jmp_rel32(&ls, JumpKind::Jmp);
                self.label(&le);
                self.loop_continue = sc;
                self.loop_break = sb;
                self.break_targets.pop();
            },
            Stmt::Do { body, cond, .. } => {
                let ls = self.fresh_label();
                let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone());
                self.loop_break = Some(le.clone());
                self.break_targets.push((le.clone(), false));
                self.maybe_align_loop();
                self.label(&ls);
                self.gen_stmt(body);
                self.gen_expr(cond, 0);
                self.emit_truth_flags(&cond.ty());
                self.jmp_rel32(&ls, JumpKind::Jne);
                self.label(&le);
                self.loop_continue = sc;
                self.loop_break = sb;
                self.break_targets.pop();
            },
            Stmt::Block { b, .. } => {
                let cleanup_base = self.cleanup_vars.len();
                self.gen_block_items(&b.items);
                // P2-9: 块结束时逆序调用 cleanup 函数
                while self.cleanup_vars.len() > cleanup_base {
                    if let Some((var_name, fn_name)) = self.cleanup_vars.pop() {
                        // 发射: lea rdi, [r13 + offset]; call fn_name
                        let off = self.var_offset(&var_name);
                        self.emit_lea_r13(7, off); // rdi = &var
                        self.call_label(&fn_name);
                    }
                }
                // 含 VLA 的块 fall-through 退出: 恢复动态栈基,
                // 丢弃本块及更深层已退出的动态分配 (见 emit_vla_restore)
                if Self::block_contains_vla(&b.items) {
                    self.emit_vla_restore();
                }
            },
            Stmt::Break { .. } => {
                // break 跳到最近封闭的 switch / 循环出口。switch 的
                // 出口标签 l_break 会统一弹出 switch 值 (add rsp, 8),
                // 因此这里只发跳转, 不重复弹栈。
                self.emit_vla_restore();
                if let Some((l, _)) = self.break_targets.last().cloned() {
                    self.jmp_rel32(&l, JumpKind::Jmp);
                }
            },
            Stmt::Continue { .. } => {
                // 回跳前恢复动态栈基: continue 丢弃本轮迭代内的 VLA
                self.emit_vla_restore();
                if let Some(l) = self.loop_continue.clone() {
                    self.jmp_rel32(&l, JumpKind::Jmp);
                }
            },
            Stmt::Goto { target, .. } => {
                // goto 可能跳出含 VLA 的块或回跳到声明前, 跳转前统一
                // 恢复动态栈基, 防止回跳循环累积泄漏 (栈耗尽段错误)
                self.emit_vla_restore();
                // Non-local goto: 嵌套函数 goto 到父函数中的标签.
                // 标签不在当前函数体内定义 → 需要恢复父帧指针.
                // r10 (静态链) 指向父帧, 将其加载到 r13 (帧指针).
                let is_nonlocal =
                    self.current_func_is_nested && !self.local_labels.contains(&target.name);
                if is_nonlocal {
                    // mov r13, r10  (恢复父帧指针)
                    // 编码: 4D 89 D5
                    //   REX.WRB = 0x4D (W=1, R=1 for r10, B=1 for r13)
                    //   opcode  = 0x89 (mov r/m64, r64)
                    //   ModRM   = 0xD5 (mod=11, reg=r10&7=010, r/m=r13&7=101)
                    self.code.push(0x4D);
                    self.code.push(0x89);
                    self.code.push(0xD5);
                }
                let lbl = self.c_label_to_asm(&target.name);
                self.jmp_rel32(&lbl, JumpKind::Jmp);
            },
            Stmt::Label { name, stmt, .. } => {
                let lbl = self.c_label_to_asm(&name.name);
                self.label(&lbl);
                self.gen_stmt(stmt);
            },
            // C++ 零成本异常: 受保护调用。LSDA call-site 区间就是被包装调用语句
            // 的地址范围 —— 表驱动展开以 PC 为键, 区间越接近「一条调用指令」,
            // 落地垫能做出的清理决定就越精确 (少算则泄漏, 多算则重复析构)。
            Stmt::Invoke {
                call,
                landing_pad,
                actions,
                ..
            } => {
                let start = self.pos();
                self.gen_stmt(call);
                let end = self.pos();
                self.eh_sites
                    .push(crate::codegen::x86_64::eh_tables::EhSite {
                        start,
                        end,
                        landing_pad: landing_pad
                            .as_ref()
                            .map(|pad| self.c_label_to_asm(&pad.name)),
                        actions: actions.clone(),
                    });
            },
            // C++ 零成本异常: 落地垫序言。unwinder 把 IP 直接交到标签处, 此时按
            // x86-64 C++ ABI: %rax = 异常对象指针, %rdx = type selector。
            // 这两个寄存器值只在本序列之后仍然有效, 因此贴完标签就立即溢出到槽位;
            // 剩下的清理 / 按 selector 分派 / `__cxa_begin_catch` 都是普通语句。
            Stmt::EhLandingPad {
                label,
                object,
                selector,
                ..
            } => {
                let lbl = self.c_label_to_asm(&label.name);
                self.label(&lbl);
                let slots = match (&**object, &**selector) {
                    (Expr::Ident { name: o, .. }, Expr::Ident { name: s, .. }) => {
                        Some((o.name.clone(), s.name.clone()))
                    },
                    _ => None,
                };
                let Some((o_name, s_name)) = slots else {
                    self.diagnostics.push(crate::diagnostics::Diagnostic {
                        sev: crate::diagnostics::Severity::Error,
                        code: "E1121",
                        msg: "exception landing pad slots must be named local variables"
                            .to_string(),
                        span: crate::diagnostics::Span::point("<eh>", 0, 0),
                        hint: Some("后端只能在落地垫第一条指令把 rax/rdx 存入具名栈槽".into()),
                        source_line: None,
                    });
                    return;
                };
                // rax = 0, rdx = 2; 两个槽都是指针/整数宽度 (8 字节)
                let obj_off = self.var_offset(&o_name);
                self.store_stack(0, obj_off);
                let sel_off = self.var_offset(&s_name);
                self.store_stack(2, sel_off);
            },
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式, 比较链跳转到对应 case 体.
                // 布局: 所有比较集中在开头 (相等跳 l_case_i), 无匹配
                // 跳 default / break 出口; case 体按源码顺序物理排列,
                // fall-through (case 体末尾无 break) 自然落入下一 case 体.
                let l_end = self.fresh_label();
                let l_default = self.fresh_label();
                let l_break = self.fresh_label();
                let mut has_default = false;
                // break 目标栈: switch 压入 (l_break, false)。switch 值
                // 保存在帧内槽, break 跳转无需额外清理。
                self.break_targets.push((l_break.clone(), false));
                // 计算switch表达式到rax
                self.gen_expr(expr, 0);
                let switch_is_int128 = matches!(expr.ty(), Type::Int128 | Type::UInt128);
                // switch 值保存到 r13 帧内槽而非 push 栈槽: case 体中
                // 可能含函数调用, push 跨越调用会使 rsp 错位 8, 违反
                // 16 字节栈对齐 ABI (pr44575: case 体中 call printf
                // 时 libc 内 movaps 对齐崩溃)。
                let sw_slot = self.alloc_sret_slot(16); // int128 需低/高 16 字节
                let sw_lo_slot = self.alloc_sret_slot(8); // 范围比较 low 暂存
                if switch_is_int128 {
                    // __int128: gen_expr 已正确加载 rax(low) + rdx(high)
                    self.store_stack(0, sw_slot); // low
                    self.store_stack(2, sw_slot + 8); // high
                } else {
                    self.store_stack(0, sw_slot); // 保存switch值
                }
                // 判断 switch 表达式是否有符号类型 (决定范围比较使用
                // 有符号 jl/jg 还是无符号 jb/ja 跳转指令).
                let switch_is_signed = match expr.ty() {
                    Type::Enum(_, _, Some(inner)) => Self::is_signed_int(&inner.base),
                    ref ty => Self::is_signed_int(ty),
                };
                let mut case_bodies: Vec<(&crate::ast::Case, String)> = Vec::new();
                for case in cases {
                    if let Some(ref val) = case.val {
                        let l_case = self.fresh_label();
                        if let Some(ref hi) = case.range_end {
                            // GNU case low ... high: 范围匹配
                            // 保存 low 到临时栈槽, 防止 gen_expr(hi) 求值时
                            // 将 rdx 用作临时寄存器导致 low 值被覆盖.
                            let l_next = self.fresh_label();
                            self.gen_expr(val, 2); // rdx = low
                            self.store_stack(2, sw_lo_slot);
                            self.gen_expr(hi, 8); // r8 = high
                                                  // Bug fix: gen_expr 可能将 rcx 用作临时寄存器,
                                                  // 每次比较前从帧内槽重新加载 switch 值到 rcx.
                            self.load_stack(1, sw_slot); // rcx = switch 值
                            self.load_stack(2, sw_lo_slot); // rdx = low
                            self.emit(&[0x48, 0x39, 0xD1]); // cmp rcx, rdx
                                                            // 有符号用 jl (skip if < low), 无符号用 jb
                            let jk_lo = if switch_is_signed { JumpKind::Jl } else { JumpKind::Jb };
                            self.jmp_rel32(&l_next, jk_lo);
                            // 重新加载 switch 值 (gen_expr(hi) 可能覆写 rcx)
                            self.load_stack(1, sw_slot); // rcx = switch 值
                            self.emit(&[0x4C, 0x39, 0xC1]); // cmp rcx, r8
                                                            // 有符号用 jg (skip if > high), 无符号用 ja
                            let jk_hi = if switch_is_signed { JumpKind::Jg } else { JumpKind::Ja };
                            self.jmp_rel32(&l_next, jk_hi);
                            self.jmp_rel32(&l_case, JumpKind::Jmp);
                            self.label(&l_next);
                        } else {
                            // case N: 比较 rcx == N, 相等跳 case 体
                            if switch_is_int128 {
                                // __int128 case 比较: 纯寄存器方式
                                // [sw_slot] = switch low, [sw_slot+8] = switch high
                                let case_low: i64 = if let Ok(cv) = const_eval::eval(val) {
                                    cv as i64
                                } else {
                                    0
                                };
                                let case_high: i64 = if let Ok(cv) = const_eval::eval(val) {
                                    (cv >> 64) as i64
                                } else {
                                    0
                                };
                                // 比较低 64 位: switch_low vs case_low
                                self.load_stack(0, sw_slot); // rax = switch low
                                if case_low == 0 {
                                    self.emit(&[0x48, 0x85, 0xC0]); // test rax, rax
                                } else {
                                    // mov rdx, case_low; cmp rax, rdx
                                    self.emit(&[0x48, 0xBA]); // mov rdx, imm64
                                    self.emit(&case_low.to_le_bytes());
                                    self.emit(&[0x48, 0x39, 0xD0]); // cmp rax, rdx
                                }
                                let l_nomatch = self.fresh_label();
                                self.jmp_rel32(&l_nomatch, JumpKind::Jne);
                                // 低 64 位匹配, 检查高 64 位
                                self.load_stack(9, sw_slot + 8); // r9 = switch high
                                if case_high == 0 {
                                    self.emit(&[0x4D, 0x85, 0xC9]); // test r9, r9
                                } else {
                                    // mov r8, case_high; cmp r9, r8
                                    self.emit(&[0x49, 0xB8]); // mov r8, imm64
                                    self.emit(&case_high.to_le_bytes());
                                    self.emit(&[0x4C, 0x39, 0xC1]); // cmp r9, r8
                                }
                                self.jmp_rel32(&l_case, JumpKind::Je);
                                self.label(&l_nomatch);
                            } else {
                                self.gen_expr(val, 2);
                                // gen_expr 可能覆写 rcx, 比较前重新加载
                                self.load_stack(1, sw_slot); // rcx = switch 值
                                self.emit(&[0x48, 0x39, 0xD1]);
                                self.jmp_rel32(&l_case, JumpKind::Je);
                            }
                        }
                        case_bodies.push((case, l_case));
                    } else {
                        // default: 记录标签
                        has_default = true;
                    }
                }
                // 没有 case 匹配: 跳 default 或 break 出口 (出口统一弹栈)
                if has_default {
                    self.jmp_rel32(&l_default, JumpKind::Jmp);
                } else {
                    self.jmp_rel32(&l_break, JumpKind::Jmp);
                }
                // case 体按顺序生成, 无 break 时 fall-through 自然成立
                for (case, l_case) in case_bodies {
                    self.label(&l_case);
                    // case 体完整生成 (含局部变量声明), 与普通块一致
                    self.gen_block_items(&case.body.items);
                }
                // 生成default体
                if has_default {
                    self.label(&l_default);
                    for case in cases {
                        if case.val.is_none() {
                            self.gen_block_items(&case.body.items);
                        }
                    }
                }
                // break 出口: switch 值保存在帧内槽, 无需弹栈
                self.label(&l_break);
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_end);
                self.break_targets.pop();
            },
            Stmt::GotoIndirect { expr, .. } => {
                // goto *expr: 计算目标地址并跳转
                self.gen_expr(expr, 0); // rax = 目标地址
                self.emit(&[0xFF, 0xE0]); // jmp rax
            },
            Stmt::Fallthrough { .. } => {},
            Stmt::AsmGoto {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                goto_labels,
                volatile: _,
                span,
            } => {
                // 记录 asm 语句的源行号, 使 mini-asm 警告能附带源位置
                self.current_asm_src_line = span.line;
                // 将 GCC 的 %lN / %l[name] / %c[label] 占位符解析为 C 标签名。
                // 使用数字边界检查防止 %l1 错误匹配 %l10 等多位索引。
                // 标签名前缀 .Ll_ 以避免与函数符号名冲突。
                let mut resolved_asm = asm_str.clone();
                let label_base = outputs.len() + inputs.len();
                for (i, label) in goto_labels.iter().enumerate().rev() {
                    let prefixed = self.c_label_to_asm(&label.name);
                    resolved_asm = Self::asm_replace_label_idx(&resolved_asm, i, &prefixed);
                    resolved_asm =
                        Self::asm_replace_label_idx(&resolved_asm, label_base + i, &prefixed);
                    resolved_asm = resolved_asm.replace(&format!("%l[{}]", label.name), &prefixed);
                    // %c[label]: strip immediate prefix, resolve to label
                    resolved_asm = resolved_asm.replace(&format!("%c[{}]", label.name), &prefixed);
                    // Also replace literal label definitions in asm text:
                    // "labelname:" -> ".Ll_labelname:" so they match the
                    // prefixed label names used by the codegen.
                    resolved_asm = resolved_asm
                        .replace(&format!("{}:", label.name), &format!("{}:", prefixed));
                }
                // 跨函数标签地址 + asm goto 混用警告:
                // &&label 产生的地址仅在当前函数内有效; 若作为输入操作数
                // 传给 asm goto, 跨函数边界使用会导致未定义行为。
                for inp in inputs.iter() {
                    if matches!(inp, Expr::LabelAddr { .. }) {
                        self.asm_warn(
                            "label address used in asm goto may be invalid \
                             if the label is in a different function",
                        );
                        break;
                    }
                }
                // GCC 语义: asm goto 输出约束写回仅 fall-through 路径生效;
                // 跳转分支保留原值。
                self.emit_inline_asm_goto(
                    &resolved_asm,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                );
            },
        }
    }
    /// gen_expr 语句表达式分支 (batch F5 逐字搬迁: Expr::StmtExpr arm)。
    /// 值约定: 块内最后一条表达式语句 (或 return) 结果放入 dst。
    pub fn gen_expr_stmt_expr(&mut self, e: &Expr, dst: u8) {
        if let Expr::StmtExpr { block, .. } = e {
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
                                if matches!(&init_ty, Type::Float | Type::Double | Type::LongDouble)
                                    && Self::is_integer_type(&v.ty)
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
        }
    }

    pub fn gen_block_items(&mut self, items: &[BlockItem]) {
        for it in items {
            // 每个 block-item 从当前水位开始, 结束时回收其新增槽位
            // (与 max_sret_bytes 静态扫描的 item 边界一一对应)。
            let mark = self.save_sret_watermark();
            self.gen_block_item(it);
            self.restore_sret_watermark(mark);
        }
    }

    fn gen_block_item(&mut self, it: &BlockItem) {
        match it {
            BlockItem::Stmt(s) => self.gen_stmt(s),
            BlockItem::Decl(Decl::Var(v)) => {
                // extern 声明不分配栈槽, 仅作为全局符号引用标记
                if v.is_extern && v.init.is_none() {
                    return;
                }
                // static 局部变量已在全局数据段分配,
                // 跳过栈分配 (与函数顶层遍历 L6970 一致)
                if v.is_static
                    && self
                        .static_local_map
                        .is_static_local(&self.current_func_name, &v.name.name)
                    && !self
                        .static_local_map
                        .needs_runtime_init(&self.current_func_name, &v.name.name)
                {
                    return;
                }
                if let Some(ref reg_name) = v.register_name {
                    if let Some(reg_idx) = Self::parse_reg_name(reg_name) {
                        self.reg_bindings.insert(v.name.name.clone(), reg_idx);
                    }
                }
                // P2-9: cleanup 属性变量注册
                if let Some(ref fn_name) = v.cleanup_fn {
                    self.cleanup_vars
                        .push((v.name.name.clone(), fn_name.clone()));
                }
                // 定义+声明一体的 struct/union 类型: 变量声明点即类型
                // 声明点, 触发 VM 槽绑定/静态清除 (parser 不产生独立
                // StructDef BlockItem)
                if matches!(v.ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
                    self.gen_vm_type_bind(&v.ty);
                }
                // static local 需要运行时初始化: 发射初始化代码到全局地址
                if v.is_static
                    && self
                        .static_local_map
                        .needs_runtime_init(&self.current_func_name, &v.name.name)
                {
                    if let Some(init) = &v.init {
                        self.gen_expr(init, 0);
                        self.emit_global_store(0, &v.name.name);
                    }
                    return;
                }
                self.register_var(&v.name.name, &v.ty);
                if Self::type_has_vla(&v.ty) {
                    self.gen_vla_alloc(v);
                } else if let Some(init) = &v.init {
                    if let Expr::InitList { inits, .. } = init {
                        self.gen_init_list(&v.name.name, &v.ty, inits);
                    } else {
                        let is_agg = matches!(
                            &v.ty,
                            Type::Struct(_, _, _, _)
                                | Type::Union(_, _, _, _)
                                | Type::Array(_, ArraySize::Const(_))
                                | Type::Atomic(_)
                        ) && v.ty.size() > 8;
                        if matches!(&v.ty, Type::Vector(_, _)) || is_agg {
                            let dst_ty = v.ty.clone();
                            let dst_name = v.name.name.clone();
                            self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                        } else if let Type::Complex(inner) = &v.ty {
                            self.gen_complex_init(&v.name.name, &inner.base, init);
                        } else if let Type::Imaginary(ref inner) = &v.ty {
                            self.gen_imaginary_init(&v.name.name, &inner.base, init);
                        } else if matches!(&v.ty, Type::Float | Type::Double | Type::LongDouble) {
                            // 浮点: xmm0 结果必须用 movsd/movss 存回栈, 不能用整数 mov rax
                            self.gen_expr(init, 0);
                            // int 常量初始化浮点变量: 先转换到 xmm0 (double x = 1;)
                            self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                            self.gen_float_store(&v.name.name, &v.ty);
                        } else {
                            self.gen_expr(init, 0);
                            // Implicit float→int conversion
                            let init_ty = init.ty();
                            if matches!(&init_ty, Type::Float | Type::Double | Type::LongDouble)
                                && Self::is_integer_type(&v.ty)
                            {
                                self.emit_implicit_float_to_int(0, &init_ty, &v.ty);
                            }
                            // D7: `__int128` 是 16 字节对象，且 dcc 的值模型把它
                            // 按 rdx:rax 双寄存器交付。`mov_to_stack` 对任何
                            // sz >= 8 都只写一个 8 字节 store，于是高半从未落槽：
                            // 循环体内 `__int128 v = va_arg(...)` 后的 `v >> 64`
                            // 读到的是栈残值（实测三次迭代全部返回同一个个值）。
                            // 只在“目标与源都是 128 位整型值”时补高半 store，
                            // 避开 float→__int128 转换只备 rax 的情形。
                            let store_hi = matches!(&v.ty, Type::Int128 | Type::UInt128)
                                && matches!(&init_ty, Type::Int128 | Type::UInt128)
                                && !self.reg_bindings.contains_key(&v.name.name);
                            self.mov_to_stack(0, &v.name.name);
                            if store_hi {
                                let off = self.var_offset(&v.name.name);
                                self.store_stack(2, off + 8); // rdx = 高 8 字节
                            }
                        }
                    }
                }
            },
            // 局部 struct/union 定义声明: 含 VLA 字段时在声明点
            // 求值总大小存入绑定槽 (C11 6.2.4p7), 供 sizeof(类型)
            // 读槽 (20041218-2: 声明后 n++ 不影响 sizeof)。
            // 静态类型 (重)声明则清除残留绑定。
            BlockItem::Decl(Decl::StructDef(sd)) => {
                self.gen_vm_struct_bind(sd, false);
            },
            BlockItem::Decl(Decl::UnionDef(sd)) => {
                self.gen_vm_struct_bind(sd, true);
            },
            _ => {},
        }
    }
}
