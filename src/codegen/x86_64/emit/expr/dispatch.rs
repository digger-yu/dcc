// src/codegen/x86_64/emit/expr/dispatch.rs
// gen_expr 主 dispatcher: 由单体 x86_64.rs L9887-13640 逐字搬迁。

use crate::ast::*;
use crate::codegen::shared::asm_operand::Width;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::emit::expr::utils::AddrKeep;
use crate::codegen::x86_64::insn::BoolExpect;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{
    AluOp, Operand as OpcodeOperand, ShiftAmount, ShiftOp, UnaryOp, X86_64Opcode,
};
use crate::codegen::x86_64::opcode::{CondCode, SimdArithOp, SimdWidth};
use crate::codegen::x86_64::{AggDst, Codegen, JumpKind, VaListAddress};
use crate::const_eval;

/// `DCC_DEST_ASSIGN`：赋值目标是有 GP 家的局部且赋值值无人要时，把右值直接算进那个家。
/// 默认 0 ⇒ 产物逐字节不变（由 `lvm.o`/`sqlite3.c` 的零漂移对照与全量用例把守）。
fn dest_assign_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("DCC_DEST_ASSIGN")
            .map(|s| s != "0")
            .unwrap_or(false)
    })
}

/// `DCC_RHS_PURE`：把 Binop 左值暂存的「右值不会写 dst」判据从叶子形态扩到整棵纯算术树。
/// 默认 0（保持今天的产物），由 `scripts/meta_gate.sh` 同场配判定它值多少。
fn rhs_pure_stage() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("DCC_RHS_PURE")
            .map(|s| s != "0")
            .unwrap_or(false)
    })
}

/// 帧临时槽回读（`mov <disp>(%r13), %<reg>`）的 gas 文本，上面两处回读共用一份。
fn gas_load_from_temp_slot(dst: u8, slot_off: i32) -> String {
    use crate::codegen::x86_64::emit::gas::{mem_disp as gas_mem_disp, REG64 as GAS_REG64};
    format!("mov %{}, {}", GAS_REG64[(dst & 15) as usize], gas_mem_disp(13, slot_off as i64))
}

impl Codegen {
    /// 剥掉并**发射**一条语句外面的标签外壳，返回最里面那条语句。
    ///
    /// 为什么需要它：`({ skip: g(s); })` 里最后一个 block-item 是
    /// `Stmt::Label { stmt: Stmt::Expr { g(s) } }` —— GNU 语义中标签标的就是那条
    /// 表达式语句，语句表达式的值仍然是它。发射不能省：`goto skip` / `&&skip`
    /// 要在那个标号上找到地址（未使用的标号也照常发，与 `gen_stmt` 同一形状）。
    pub(crate) fn emit_tail_labels<'a>(&mut self, s: &'a Stmt) -> &'a Stmt {
        let mut cur = s;
        loop {
            match cur {
                Stmt::Label { name, stmt, .. } => {
                    let lbl = self.c_label_to_asm(&name.name);
                    self.label(&lbl);
                    cur = stmt;
                },
                _ => return cur,
            }
        }
    }

    /// 语句表达式的最后一个 item 是表达式语句时，把它的值放进 dst。
    ///
    /// 聚合类型 (struct/union) 的 StmtExpr 最后一条处理:
    /// - lvalue (Ident/Member/Index/Deref): 取地址 + 整体拷贝到 sret 临时槽
    ///   (不能 gen_expr 加载前 8 字节值, 调用方期望地址:
    ///    pr c/5354: ({ struct S s; s.a=1; s; }).a)
    /// - Call: SysV ABI >16B 返回已走 sret 约定 (rax=buffer 地址),
    ///   否则物化寄存器返回值到临时槽
    /// - 标量/其他: 正常 gen_expr
    pub(crate) fn gen_stmt_expr_value(&mut self, e: &Expr, dst: u8) {
        let e_ty = e.ty();
        if matches!(e_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
            let sz = e_ty.size();
            if self.gen_lvalue_addr(e, 6) {
                // lvalue 路径: 整体拷贝到 sret 临时槽
                // rsi 已 = 源地址 (gen_lvalue_addr 放到 dst=6 即 rsi)
                let temp_off = self.alloc_sret_slot(sz);
                self.emit_lea_r13(7, temp_off); // rdi = sret 临时槽
                self.mov_imm(1, sz as i128); // rcx = size
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // rep movsb
                self.emit_lea_r13(dst, temp_off);
            } else if Self::needs_memory_return(&e_ty) {
                // Call + >16B: SysV ABI sret, rax 已是返回地址
                self.gen_expr(e, dst);
            } else if matches!(e, Expr::Call { .. }) {
                // Call + <=16B: 寄存器返回, 物化到临时槽
                self.gen_expr(e, Self::EVAL_BASE);
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
    }

    /// 表达式求值的落点寄存器（Phase 5.1 前置一：求值基址单源）。真源在
    /// `abi::regbank::EVAL_BASE_REG`，本别名只为调用点写得出来而不必再抄一个字面量：
    /// `gen_expr(e, Self::EVAL_BASE)` 与以前那个 `0` 字节上相等，但「结果在哪」从此
    /// 只有一个地方可以回答。
    pub const EVAL_BASE: u8 = crate::codegen::x86_64::abi::regbank::EVAL_BASE_REG;

    /// 求值一个表达式并把结果留在**求值基址**上，同时把这件事记成可查的持有事实。
    ///
    /// 它是「结果恒在 %rax」这条约定从**口径**变成**数据**的那个点：调用方拿到的不是一个
    /// 需要它自己知道的返值，而是一条可以在发射期被问、并且会在任何一条新指令发出后
    /// 自动作废的记录（失效点是 `Codegen::emit`）。前置二（程序点级家存活）与前置三
    /// （caller-saved 保存/恢复）需要的就是这份可查事实：没有它，「这个寄存器里现在握着
    /// 谁的值」只能靠每个调用点自己记得。
    pub fn gen_expr_value(&mut self, e: &Expr) -> crate::location::RegId {
        self.gen_expr(e, Self::EVAL_BASE);
        let held = crate::location::RegId {
            bank: crate::location::RegBank::Gp,
            n: Self::EVAL_BASE,
        };
        // 必须在求值**之后**写：gen_expr 内部发了很多指令，它们都已把上一轮的记录清空了。
        self.eval_holding = Some(held);
        held
    }

    pub fn gen_expr(&mut self, e: &Expr, dst: u8) {
        // 「条件只用来决定分支」的令牌在本分发函数的**第一行**被消费。
        //
        // 必须是第一次行而不是 Binop 臂里：分支发射点发令牌后，条件本体可能是
        // `f(a<b)`、`arr[k<2] + 1`、向量/复数取读……这些路径自己不再回到 Binop 臂的入口，
        // 如果只在整型比较臂里才拿走令牌，它就会泄到那些**嵌套**比较上：嵌套比较只留
        // flags、不材料化 0/1，而它的值在其父表达式里是要用的 —— 那是静默错值而不只是
        // 少优化一条。拿走后只有「本次求值正好就是那个比较」的 Binop 臂能用它。
        let token_here = std::mem::take(&mut self.cmp_flags_token);
        // 「值没人要」的上上下文（阶段1）：入口消费，嵌套求值一律看不到。本臂只有在
        // **直接调用**它前重新置回才能看到它（中间已经做过操作数的 gen_expr，那边拿走了）。
        let dead_here = std::mem::take(&mut self.value_is_dead);
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
                // __int128 / unsigned __int128 常量: 高 64 位必须加载到 rdx,
                // 否则上半部为垃圾寄存器值 (pr61375: mask = 0xffff<<56 折叠后
                // hi=0xFFFF, 但 mov_imm 只写 rax, AND 时 hi 半部取错值)。
                if matches!(ty, Type::Int128 | Type::UInt128) {
                    self.mov_imm(2, (masked >> 64) as i128);
                }
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
                // 条件直生分支的放行判据（令牌已由 `gen_expr` 入口消费）。
                //
                // 只放行「比较 + 整数/指针操作数 + 非位域」：浮点（`ucomisd` 的无序分支是
                // PF 与目标条件的复合）、复数、向量、`__int128` 在下方各自分流；位域在
                // 通用路径尾部还要按宽度发一条掩码，那条 emit 会把 flags 清掉。
                //
                // 为什么放在臂的**第一位**而不是比较臂里：常量折叠/`&&`/`||`/指针缩放
                // 都会在此之前 `return`，它们不交回条件码，但必须把令牌吐掉，否则
                // 它们自己的操作数求值会拿到一个不属于它们的令牌。
                let hand_off = token_here
                    && matches!(
                        *op,
                        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne
                    )
                    && Self::cmp_flags_types_ok(&l.ty())
                    && Self::cmp_flags_types_ok(&r.ty())
                    && Self::expr_bitfield_width(l).is_none()
                    && Self::expr_bitfield_width(r).is_none();
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
                    && !Self::expr_has_side_effects(e)
                {
                    if let Ok(val) = const_eval::eval(e) {
                        self.mov_imm(dst, val);
                        // __int128 / unsigned __int128 折叠常量: 高 64 位写入 rdx,
                        // 与 Expr::Int 字面量路径一致 (pr61375: mask = 0xffff<<56)。
                        if matches!(binop_ty, Type::Int128 | Type::UInt128) {
                            self.mov_imm(2, (val >> 64) as i128);
                        }
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
                    use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                    use crate::codegen::x86_64::emit::gas::REG8 as GAS_REG8;
                    if matches!(l.ty(), Type::Int128 | Type::UInt128) {
                        // or dst64, rdx (REX.W 09 /r, r/m=dst reg field=rdx(2))
                        let or_gas = format!("or %rdx, {}", GAS_REG64[(dst & 15) as usize]);
                        self.emit_insn(
                            &Insn::alu_rm_reg_reg(AluOp::Or, Width::B64, dst, 2).with_gas(or_gas),
                        );
                    }
                    // test dst64, dst64 (REX.W 85 /r, reflexive ModRM)
                    let test_gas = format!("test {0}, {0}", GAS_REG64[(dst & 15) as usize]);
                    self.emit_insn(&Insn::test_rm_reg(Width::B64, dst, dst).with_gas(test_gas));
                    match op {
                        BinOp::AndAnd => {
                            self.jmp_rel32(&l_false, JumpKind::Je);
                            self.gen_expr_decay(r, dst);
                            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                                let or_gas = format!("or %rdx, {}", GAS_REG64[(dst & 15) as usize]);
                                self.emit_insn(
                                    &Insn::alu_rm_reg_reg(AluOp::Or, Width::B64, dst, 2)
                                        .with_gas(or_gas),
                                );
                            }
                            let test_gas = format!("test {0}, {0}", GAS_REG64[(dst & 15) as usize]);
                            self.emit_insn(
                                &Insn::test_rm_reg(Width::B64, dst, dst).with_gas(test_gas),
                            );
                            // setne dst_low: 结果写入 dst 的低 8 位, 而非固定 al.
                            // `0F 95 /r` 的目的在 **r/m** ⇒ dst>=8 需要 REX.B.
                            let setne_gas = format!("setne {}", GAS_REG8[(dst & 15) as usize]);
                            self.emit_insn(&Insn::setcc_reg(CondCode::Ne, dst).with_gas(setne_gas));
                            // movzb dst64, dst_low8 (0F B6 /r reflexive)
                            let movzb_gas = format!(
                                "movzbq {}, {}",
                                GAS_REG8[(dst & 15) as usize],
                                GAS_REG64[(dst & 15) as usize]
                            );
                            self.emit_insn(
                                &Insn::mov_ext(false, Width::B8, Width::B64, dst, dst)
                                    .with_gas(movzb_gas),
                            );
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            self.mov_imm(dst, 0);
                            self.label(&l_end);
                        },
                        BinOp::OrOr => {
                            self.jmp_rel32(&l_false, JumpKind::Jne);
                            self.gen_expr_decay(r, dst);
                            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                                let or_gas = format!("or %rdx, {}", GAS_REG64[(dst & 15) as usize]);
                                self.emit_insn(
                                    &Insn::alu_rm_reg_reg(AluOp::Or, Width::B64, dst, 2)
                                        .with_gas(or_gas),
                                );
                            }
                            let test_gas = format!("test {0}, {0}", GAS_REG64[(dst & 15) as usize]);
                            self.emit_insn(
                                &Insn::test_rm_reg(Width::B64, dst, dst).with_gas(test_gas),
                            );
                            let setne_gas = format!("setne {}", GAS_REG8[(dst & 15) as usize]);
                            self.emit_insn(&Insn::setcc_reg(CondCode::Ne, dst).with_gas(setne_gas));
                            let movzb_gas = format!(
                                "movzbq {}, {}",
                                GAS_REG8[(dst & 15) as usize],
                                GAS_REG64[(dst & 15) as usize]
                            );
                            self.emit_insn(
                                &Insn::mov_ext(false, Width::B8, Width::B64, dst, dst)
                                    .with_gas(movzb_gas),
                            );
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
                    && (Self::is_scalar_float_binop_ty(&l.ty())
                        || Self::is_scalar_float_binop_ty(&r.ty()))
                {
                    if self.fp_contract && self.try_gen_fp_contract(*op, l, r, dst) {
                        return;
                    }
                    self.gen_float_binop(*op, l, r);
                    // 浮点比较结果在 rax, 需要移动到 dst
                    // `mov(dst, 0)` 是 rax→dst（与全仓 48 处 rax→dst 同形）；
                    // 旧代码写反成 `mov(0, dst)`（dst→rax），把刚算好的布尔结果
                    // 用 dst 里的陈旧家值抹掉（实测 cmp-mixed：右操作数暂存到 r8，
                    // `mov r8, rax` 成了 `mov rax, r8`，rax=1 被盖成 117）。
                    if matches!(
                        op,
                        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne
                    ) {
                        if dst != 0 {
                            self.mov(dst, 0);
                        }
                    } else if Self::is_integer_type(binop_ty) {
                        // 复合赋值脱糖形态: `int a += double d` 的 Binop.ty
                        // 标注为 LHS 整数类型 (parser.rs C99 6.5.17 计算后回
                        // 转语义), 但 gen_float_binop 的混合运算结果在 xmm0
                        // — 必须转回整数, 否则 Assign 层按 ty=整数直接存
                        // dst 中的陈旧值 (mix.c: a+=d 得 1 而非 20)。
                        // 普通表达式 `a + d` 的 ty 是通常算术转换类型
                        // (浮点), 不受影响。
                        let fl_ty = if matches!(l.ty(), Type::Float128)
                            || matches!(r.ty(), Type::Float128)
                        {
                            Type::Float128
                        } else if matches!(l.ty(), Type::LongDouble)
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
                //
                // `i + p` 必须先对调成 `p + i`：下面的 `imul` 只作用在**右**
                // 操作数上，而 `scale` 取的是指针那一侧的元素大小。指针在右时
                // 乘的就是指针本身 —— `(top - old) + newstack` 曾生成
                // `sub/idiv 16/imul newstack,16/add`，把 `L->top.p` 写成野指针
                // （lua 的 `correctstack` 因此踩栈上界断言）。Add 可交换，
                // 对调不改变语义；`i - p` 不是合法 C，无需处理。
                let (l, r) = if matches!(op, BinOp::Add)
                    && !matches!(l.ty(), Type::Ptr(_) | Type::Array(_, _))
                    && matches!(r.ty(), Type::Ptr(_) | Type::Array(_, _))
                {
                    (r, l)
                } else {
                    (l, r)
                };
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
                // 指针 ± **整数常量**：把缩放后的字节偏移并进一条 `add $imm, dst`。
                //
                // 不做这一步时 `p + 1` 会展开成「mov 常量 / movsxd / imul / add」四条，
                // 再叠上「左值先算到 dst、再搬到 callee-saved」的固定开销 —— 实测
                // luaV_execute 里每条 `base = ci->func.p + 1` 都是 8-9 条指令。
                // 常量已在编译期知道缩放结果，不需要那条 imul，也不需要为右操作数
                // 占一个暂存号。
                //
                // 只认「左操作数是指针/数组」：`1 + p` 要把两边对调才能用同一算式，
                // 那是另一条路径，不在本优化里顺手做。`r` 必须是无副作用的整型，
                // 否则 `const_eval` 对浮点会截断、对带副作用的式子可能求错值。
                if needs_scale
                    && scale > 0
                    && l_inner.is_some()
                    && Self::is_integer_type(&r.ty())
                    && !Self::expr_has_side_effects(r)
                {
                    if let Ok(v) = const_eval::eval(r) {
                        let bytes_off = (v as i64).wrapping_mul(scale as i64);
                        let delta = if matches!(op, BinOp::Sub) {
                            bytes_off.wrapping_neg()
                        } else {
                            bytes_off
                        };
                        self.gen_expr_decay(l, dst);
                        self.add_reg_byte_offset(dst, delta);
                        return;
                    }
                }
                // C11 6.3.1.8 通常算术转换: 确定运算的有符号性.
                // C11 6.3.1.1: 整数提升 (char/short → int) 必须在
                // 有符号性判定之前执行. 例如 signed char vs unsigned char
                // 提升后均为 int (有符号), 比较应使用有符号指令.
                //
                // 提升与转换都只调 [`Type`] 上的真源 (`int_promotion` / `usual_arith_type`),
                // 不在这里重复一份秩表: 曾有的本地 `type_rank` + 「只看秩」混合规则把
                // `unsigned long`(秩 4) vs `long long`(秩 5) 判成有符号, 于是无符号比较拿到
                // `setg`/`jle` —— 与 `ast.rs::usual_arith_type` 里缺少的「有符号侧能否表示
                // 无符号侧全部值」是同一根因的两处症状。两份规则各写一遍时, 修一处会留下
                // 另一处, 故这里只留一个调用点。
                let lt_promo = l.ty().int_promotion();
                let rt_promo = r.ty().int_promotion();
                // C11 6.5.7: 移位运算的有符号性仅由左操作数决定,
                // 右操作数仅为移位计数, 不参与符号判定.
                let is_signed = if matches!(op, BinOp::Shl | BinOp::Shr) {
                    Self::is_signed_int(&l.ty())
                } else {
                    // 公共类型由单一真源给出, 它的有符号性就是本条运算的有符号性。
                    Self::is_signed_int(&lt_promo.usual_arith_type(&rt_promo))
                };
                // 移位量是**整型常量**：直接 imm8 形式（`sar r64, 7`）。
                //
                // 不这样做时 `i >> 7` 要经过「求值到暂存号 → mov %cl, r → shr →
                // 搬回 dst」，外面还套一层 push/pop 与两条 `mov $0`，实测 10 条；
                // imm 形式是 1 条。而 GET_OPCODE 是**每条 VM 指令都要走**的路径
                // （`luaV_execute` 里 333 处常量移位）。
                //
                // 限制条件全部是为了与通用路径**逐位相同**：
                //   - 左操作数不是位域（位域在通用路径尾部还要按宽度掩码）；
                //   - 计数 0..=63（更大的计数是 UB，xl/imm8 只取低 5/6 位；
                //     保持原样比在这里复述那条规则安全）。`v == 0` 时连移位都不发：
                //     x86 的移位在计数为 0 时**不改标志位**，与通用路径（`shr r,cl`
                //     且 cl==0）逐位等价。这是 `GET_OPCODE` 之前那一段的来源。
                //   - 两边都是整型（浮点/向量/__int128 已在上面分流）。
                if matches!(*op, BinOp::Shl | BinOp::Shr)
                    && Self::is_integer_type(&l.ty())
                    && Self::expr_bitfield_width(l).is_none()
                    && !Self::expr_has_side_effects(r)
                {
                    if let Ok(v) = const_eval::eval(r) {
                        if v >= 0 && v <= 63 {
                            use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                            self.gen_expr_decay(l, dst);
                            if v != 0 {
                                let op_field: u8 = match *op {
                                    BinOp::Shl => 4,
                                    BinOp::Shr => {
                                        if is_signed {
                                            7
                                        } else {
                                            5
                                        }
                                    },
                                    _ => unreachable!(),
                                };
                                let mnem = match op_field {
                                    4 => "shl",
                                    5 => "shr",
                                    _ => "sar",
                                };
                                let gas = format!("{mnem} {}, {v}", GAS_REG64[(dst & 15) as usize]);
                                self.emit_insn(
                                    &Insn::shift_reg_imm8_c1_b64(op_field, dst, v as u8)
                                        .with_gas(gas),
                                );
                            }
                            // 与通用路径尾部一致：32 位有符号 Shl 要符号扩展回 64 位。
                            let res_ty = e.ty();
                            if res_ty.size() == 4
                                && res_ty.is_int()
                                && matches!(*op, BinOp::Shl)
                                && Self::is_signed_int(&res_ty)
                            {
                                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                                let gas = format!(
                                    "movsxd {}, {}",
                                    GAS_REG32[(dst & 15) as usize],
                                    GAS_REG64[(dst & 15) as usize]
                                );
                                self.emit_insn(
                                    &Insn::mov_ext(true, Width::B32, Width::B64, dst, dst)
                                        .with_gas(gas),
                                )
                                .expect("typed-opcode: mov_ext");
                            }
                            return;
                        }
                    }
                }
                // 右操作数是**整型常量**：直接 imm32 形式（`and r12, 0x7f`）。
                //
                // 不这样做时 `i & 0x7f` 要经过「左值搬到暂存号 → 把常量搬进另一个
                // 暂存号 → push/pop → and」共 7 条，而 GETARG_A/B/C 每个都要做一次。
                //
                // 等价性（与 `binop` 的位宽选择逐条对齐）：
                //   - 常量落在 i32 内（imm32 编码）；
                //   - And/Or/Xor 再要求常量非负：32 位无符号路径的通用代码发的是
                //     32 位运算（结果零扩展），而 imm32 在 64 位形式里会被符号扩展 ——
                //     非负常量下高位保持 0，两边逐位相同；
                //   - Add/Sub 不受此限：32 位结果由尾部截断拉回原语义；
                //   - 左操作数不是位域（位域在通用路径尾部还要按宽度再掩码）。
                if matches!(*op, BinOp::Add | BinOp::Sub | BinOp::And | BinOp::Or | BinOp::Xor)
                    && Self::is_integer_type(&l.ty())
                    && Self::expr_bitfield_width(l).is_none()
                    && !Self::expr_has_side_effects(r)
                {
                    if let Ok(v) = const_eval::eval(r) {
                        let op_ext: u8 = match *op {
                            BinOp::Add => 0,
                            BinOp::Or => 1,
                            BinOp::And => 4,
                            BinOp::Sub => 5,
                            BinOp::Xor => 6,
                            _ => unreachable!(),
                        };
                        let bitwise = matches!(*op, BinOp::And | BinOp::Or | BinOp::Xor);
                        if v >= i32::MIN as i128 && v <= i32::MAX as i128 && (!bitwise || v >= 0) {
                            self.gen_expr_decay(l, dst);
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(op_ext, dst, v as i64));
                            // 32 位结果的截断，与通用路径尾部一致。
                            let res_ty = e.ty();
                            if res_ty.size() == 4
                                && res_ty.is_int()
                                && matches!(*op, BinOp::Add | BinOp::Sub)
                            {
                                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                                if Self::is_signed_int(&res_ty) {
                                    let gas = format!(
                                        "movsxd {}, {}",
                                        GAS_REG32[(dst & 15) as usize],
                                        GAS_REG64[(dst & 15) as usize]
                                    );
                                    self.emit_insn(
                                        &Insn::mov_ext(true, Width::B32, Width::B64, dst, dst)
                                            .with_gas(gas),
                                    )
                                    .expect("typed-opcode: mov_ext");
                                } else {
                                    let gas = format!(
                                        "mov {}, {}",
                                        GAS_REG32[(dst & 15) as usize],
                                        GAS_REG32[(dst & 15) as usize]
                                    );
                                    let _ = self.emit_insn(
                                        &Insn::mov_rm_reg(Width::B32, dst, dst).with_gas(gas),
                                    );
                                }
                            }
                            return;
                        }
                    }
                }
                // 比较的右操作数是**整型常量**：直接 `cmp dst, imm`（0 用 `test`）。
                //
                // 不这样做时右操作数要先 `mov $imm, %stage` —— `x != 0` 就是一条
                // `mov $0` 加一条 `cmp %dst, %stage`，而 0 是所有宏展开（`lua_assert`、
                // 指针空判、状态码比较）里最常见的比较对象（lvm.c 实测 812 条）。
                //
                // 等价性（与 `binop` 的 cmp 分支逐条对齐）：
                //   - 位宽判定同一条：两边都 ≤ 4 字节 → 32 位比较；
                //   - 常量已由 `gen_expr` 的 Int/Char 路径按 rt 做过 mask（符号/零扩展），
                //     所以通用路径的 `zero_extend_for_cmp(stage, rt)` 对它是恒等；
                //   - 32 位比较取常量低 32 位（`cmp r/m32, imm32` 本就只比 32 位）；
                //     64 位比较要求 mask 落在 i32 内（imm32 在 64 位形式里符号扩展），
                //     超出则退回通用路径；
                //   - 右操作数必须无副作用（白名单外一律不放行，避免跳过求值）；
                //   - 左操作数仍需按通用路径做 `zero_extend_for_cmp`（无符号宽比较）。
                if matches!(
                    *op,
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne
                ) && Self::is_integer_type(&l.ty())
                    && Self::is_integer_type(&r.ty())
                    && !Self::expr_has_side_effects(r)
                {
                    if let Ok(v) = const_eval::eval(r) {
                        let use_32bit = l.ty().size() <= 4 && r.ty().size() <= 4;
                        let masked = Self::mask_imm_to_type(v, &r.ty());
                        let imm: i64 = if use_32bit {
                            (masked as u32 as i32) as i64
                        } else {
                            masked as i64
                        };
                        // B64 + dst==rsp 的 AluRm imm 分支在编码器里只认 Add/Sub，
                        // 其余返回 None（静默丢 cmp）；非零立即数遇 rsp 直接放弃。
                        let encodable = imm == 0 || use_32bit || dst != 4;
                        if (use_32bit || (masked >= i32::MIN as i128 && masked <= i32::MAX as i128))
                            && encodable
                        {
                            self.gen_expr_decay(l, dst);
                            if !is_signed && !use_32bit {
                                self.zero_extend_for_cmp(dst, &l.ty());
                            }
                            self.cmp_flags_granted = hand_off;
                            self.emit_cmp_imm(*op, dst, is_signed, use_32bit, imm);
                            self.cmp_flags_granted = false;
                            return;
                        }
                    }
                }
                // 统一路径: 被调用者保存寄存器优先, 帧临时槽后备.
                // 使用 gen_expr_decay: 数组标识符在运算中衰减为指针
                // binop 临时槽: 放在 callee-saved 区域下方
                // 布局: r13+0 saved r13 / r13+8 ret addr
                //       / r13-16..r13-48 callee-saved save area (rbx/r12/r14/r15)
                //       / r13-48..-(48+temp*8) binop temp_slots (槽 d 在 -(48+d*8))
                //       / r13-(48+temp*8).. 变量 (基址 hdr 含 temp_slots)
                // 注意: binop 槽公式只用 48 (callee-saved 区大小),
                // temp_slots 只体现在变量基址 hdr 中, 不可重复计入.
                let slot_off = crate::codegen::x86_64::abi::layout::LayoutMap::temp_slot_off(
                    self.expr_depth as i64,
                );
                // 尝试使用被调用者保存寄存器代替帧槽 (减少内存访问).
                // callee-saved 跨 call 保活 (ABI 保证 + 本编译器序言保存
                // callee_saved_used 集合), 因此含 call 的表达式同样安全;
                // 递归 binop 通过 `RegBank::alloc_temp` 分配，不冲突.
                // dst==1 (rcx) 时左值可能是 shift 计数, 保守退回帧槽.
                let use_callee_saved = dst != 1;
                // 右操作数是「只会写自己那个暂存号」的形态（字面量 / 名字）时，
                // 求值过程不可能碰到 `dst`，左操作数就**不必搬走**。
                //
                // 这一步同时是寄存器池的节流阀：每次 Binop 都占一个 callee-saved 号，
                // 四个池号被函数级变量家占满时就只能退到帧槽（实测给 4 个变量绑家后
                // 栈引用反增 1518 条，净收益只剩 -297 条）。名字与字面量占了
                // `luaV_execute` 里交互的大多数（`ra + 1` / `pc + n` / `k + i`），
                // 把它们从池里摘出去，池才能留给真的需要跨求值保活的那些。
                //
                // 白名单而不是「无副作用」：`expr_has_side_effects` 对 `StmtExpr`
                // 与内置函数返回 false（它的 `_ => false` 是默认臂），而那两类里
                // 完全可能有 call。喊错一条就是静默错值。
                let rhs_isolated =
                    matches!(r.as_ref(), Expr::Int { .. } | Expr::Char { .. } | Expr::Ident { .. });
                // `dst == 1`（rcx）那一支原本就强制存（左值可能是 shift 计数），旋钮开不开
                // 都不变；`rhs_only_writes_stage` 也已经自己排掉了需 `%cl` 的变量移位。
                let rhs_pure = rhs_pure_stage() && dst != 1 && Self::rhs_only_writes_stage(r);
                let need_save_lhs = !(rhs_isolated || rhs_pure) || dst == 1;
                // 诊断（与 `DCC_STAGE_PROBE` 同风格）：左值要溅帧槽时，把右值树的形状前两级
                // 打出来聚合。不写这个就只能猜「为什么 248 条链里只消掉了 13 条」。
                if need_save_lhs && std::env::var_os("DCC_RHS_PROBE").is_some() {
                    let s = format!("{r:?}");
                    let cut = s
                        .find(", ty:")
                        .unwrap_or_else(|| s.find("span:").unwrap_or(s.len()));
                    eprintln!(
                        "RHSPROBE dst={dst} iso={rhs_isolated} pure={rhs_pure} {}",
                        &s[..cut]
                    );
                }
                let callee_reg = if use_callee_saved && need_save_lhs {
                    self.alloc_callee_saved()
                } else {
                    None
                };
                self.gen_expr_decay(l, dst);
                // 保存左操作数: 优先到被调用者保存寄存器, 否则到帧槽
                if let Some(reg) = callee_reg {
                    self.mov(reg, dst);
                } else if need_save_lhs {
                    // mov [r13+slot_off], dst (r13=13)
                    //
                    // 挂 gas 与本文件其他地方同口径：`-S` 文本与字节必须同源，否则
                    // P12 的文本平价检查（scripts/check_asm_text_parity.sh）对这一类
                    // 高频发射点只能靠默认反推。
                    use crate::codegen::x86_64::emit::gas::{
                        mem_disp as gas_mem_disp, REG64 as GAS_REG64,
                    };
                    let gas = format!(
                        "mov {}, %{}",
                        GAS_REG64[(dst & 15) as usize],
                        gas_mem_disp(13, slot_off as i64)
                    );
                    self.emit_insn(
                        &Insn::mov_rm_mem(false, Width::B64, dst, 13, slot_off).with_gas(gas),
                    );
                }
                self.expr_depth += 1;
                // 右操作数的暂存号。以前这里与下面三处字节都写死 `1`（`%rcx`），意思就是
                // 「rcx 永远归暂存」—— 这条约定凌驾在占用事实上，所以把 caller-saved 号
                // 发给一个变量当家后，紧跟其后的求值会把那个家踩掉（实测 `a+b+c+d+e` 里
                // `c` 的家给 `%rcx` ⇒ gcc 50 / dcc 47，设计 §11.40）。
                //
                // 取号器 `RegBank::pick_rhs_stage` 读的就是占用事实；无人握着时仍返首位 `1`，
                // 所以今天的产物逐字节不变（三档哈希是裁判）。`dst == 1` 那条 push/pop 特殊路
                // 原样保留（那是已有形状，不顺手改）。
                //
                // 三级取号 + 一个必修的泄漏（2026-10，实测 `lvm.c` 串行编译 E1135）：
                //   1. `pick_rhs_stage`：候选表 {r8,r9} 里无人握着的号，不占 callee-saved 池；
                //   2. `alloc_temp`：池里第一个空位（表达式溅出槽）——**旧实现借了不还**，
                //      于是每层嵌套 Binop 永久消耗一个池号，四个之后池枯竭，与家号钉住
                //      （`stage_free_after` 只数了候选表、没数池）合谋触发 E1135；
                //   3. 借用 `%rcx`：家只可能来自 POOL 与延伸池（均不含 rcx），用户显式
                //      `register asm("rcx")` 钉住的号在 `pinned` 里，一并排除。rcx 是本
                //      后端历史上的固定暂存号，Binop 各分支（含 Div 的 cqto/idiv、移位
                //      的 %cl）都建立在「rcx 可当暂存」上，所以借它不需要新路径；
                //      借 `%rdx` 不行：Div 的 `src == 2` 分支是死代码且除的是 rcx 而非
                //      存好的除数，激活它就是激活一条错值路径。
                // `dst == 1` 走第 454 行的 push/pop 特例，永远不进这里。
                let mut stage_from_pool: Option<u8> = None;
                let stage = if dst == 1 {
                    1
                } else {
                    let rhs_names = crate::codegen::x86_64::abi::regbank::rhs_stage_candidates();
                    let mut picked: Option<u8> = None;
                    if let Some(n) = self.reg_bank.pick_rhs_stage(dst) {
                        picked = Some(n);
                    } else if let Some(n) = self.reg_bank.alloc_temp_except(Some(dst)) {
                        picked = Some(n);
                        stage_from_pool = Some(n);
                    } else if !self.reg_bank.is_pinned(1) {
                        picked = Some(1);
                    }
                    match picked {
                        Some(n) => n,
                        None => {
                            // 候选表、临时号池与借用号全不可用：不得退到一个正握着家的号上。
                            // 诊断里点名「谁握着这些号」：不写就只能靠猜，而本仓已经因为
                            // 猜错过两次（§11.45/§11.46）。
                            let holders: Vec<String> = rhs_names
                                .iter()
                                .map(|&c| {
                                    format!(
                                        "{c}(alloc={},pinned={})",
                                        self.reg_bank.is_allocated(c),
                                        self.reg_bank.is_pinned(c)
                                    )
                                })
                                .collect();
                            self.report_internal_bug(
                                "E1135",
                                format!(
                                    "右操作数暂存取不到空闲号（dst={dst}）：候选表与临时号池全被握着；占用情况 {}",
                                    holders.join(" ")
                                ),
                                "扩 RHS 候选集，或让有硬用途的号不进候选表；不得静默拿有主的号",
                            );
                            1
                        },
                    }
                };
                self.gen_expr_decay(r, stage);
                self.expr_depth -= 1;
                // 对整数操作数乘以指针所指类型大小 (跳过 char* 和 ptr-ptr)
                if scale > 1 {
                    // 有符号 32 位或更小整数需先符号扩展到 64 位, 否则
                    // 负值零扩展后成为大正数, 乘积错误 (pr44575 类 bug)
                    let r_ty = r.ty();
                    if Self::is_signed_int(&r_ty) && r_ty.size() <= 4 {
                        // movslq r64, r32（自反 ModRM）
                        use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                        use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                        let gas = format!(
                            "movsxd {}, {}",
                            GAS_REG32[(stage & 15) as usize],
                            GAS_REG64[(stage & 15) as usize]
                        );
                        self.emit_insn(
                            &Insn::mov_ext(true, Width::B32, Width::B64, stage, stage)
                                .with_gas(gas),
                        )
                        .expect("typed-opcode: mov_ext");
                    }
                    // imul r64, r64, imm32（自反 ModRM）
                    self.emit_insn(&Insn::imul_rm_reg_imm32_b64(stage, scale as i32))
                        .expect("typed-opcode: imul_rm_reg_imm32_b64");
                }
                // 比较臂可以只发 cmp 并把条件码留给 `last_cmp`：紧靠调用点置位/复位，
                // 中间不插入任何其它表达式求值，所以不会溢出到别的表达式。
                self.cmp_flags_granted = hand_off;
                if let Some(reg) = callee_reg {
                    // 被调用者保存寄存器路径: 左操作数在 reg, 右操作数在 stage。
                    self.binop(*op, reg, stage, is_signed, &l.ty(), &r.ty());
                    // 条件直生成功时不搬结果：dst 里不需要值（flags 就是值），而那条
                    // `mov` 会把刚比较出来的 flags 清掉（`last_cmp` 只可能由比较臂写入）。
                    if reg != dst && self.last_cmp.is_none() {
                        self.mov(dst, reg);
                    }
                    self.free_callee_saved(reg);
                } else if !need_save_lhs {
                    // 左操作数一直在 dst（右操作数的求值不会写它）。
                    self.binop(*op, dst, stage, is_signed, &l.ty(), &r.ty());
                } else if dst == 1 {
                    // 帧槽路径, dst == rcx: push/pop 避免恢复覆盖右操作数
                    self.push(1); // push rcx (右操作数)
                                  // 恢复左操作数到 dst: mov dst, [r13+slot_off] (r13=13)
                    self.emit_insn(
                        &Insn::mov_rm_mem(true, Width::B64, dst, 13, slot_off)
                            .with_gas(gas_load_from_temp_slot(dst, slot_off)),
                    );
                    self.pop(2); // pop rdx (右操作数)
                    self.binop(*op, dst, 2, is_signed, &l.ty(), &r.ty());
                } else {
                    // 帧槽路径: 恢复左操作数 (mov dst, [r13+slot_off])
                    self.emit_insn(
                        &Insn::mov_rm_mem(true, Width::B64, dst, 13, slot_off)
                            .with_gas(gas_load_from_temp_slot(dst, slot_off)),
                    );
                    self.binop(*op, dst, stage, is_signed, &l.ty(), &r.ty());
                }
                self.cmp_flags_granted = false;
                // ptr - ptr: 字节差 ÷ 元素大小 = 元素数.
                // idiv 固定以 rax 为被除数, 而 binop 结果在 dst:
                // dst != 0 时须先搬 rax, 除完再把商搬回 dst.
                if is_ptr_sub && elem_size > 1 {
                    if dst != 0 {
                        self.mov(0, dst); // rax = 字节差
                    }
                    // 除数也用同一个暂存号：写死 `1` 会在 `1` 握着某个家时把家踩掉。
                    self.mov_imm(stage, elem_size as i128);
                    self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                        crate::codegen::shared::asm_operand::Width::B64,
                    )); // cqo (符号扩展 rax→rdx:rax)
                        // idiv r/m64: reg field = /7 opcode extension, only r/m holds stage.
                    self.emit_insn(&Insn::idiv_rm_reg_b64(stage))
                        .expect("typed-opcode: idiv_rm_reg_b64");
                    if dst != 0 {
                        self.mov(dst, 0); // dst = 商
                    }
                }
                // 池里借的暂存号用完即还（旧实现漏了这一步：每层嵌套 Binop 永久占一个
                // 池号，四个之后池枯竭，后续 Binop 只能走帧槽路径甚至触发 E1135）。
                // 释放放在 ptr-sub 的 idiv 之后 —— 那是 stage 最后一次被读。
                if let Some(r) = stage_from_pool {
                    self.free_callee_saved(r);
                }
                // 条件直生分支：flags 已经交给分支发射点，值不必材料化。
                // 下面的 32 位截断与位域掩码都会 emit —— 那会把 flags 清掉而 dst 里
                // 又没有可用的值，所以本臂到这里必须是终点。
                if hand_off && self.last_cmp.is_some() {
                    return;
                }
                // C 语义: 32 位整数运算结果需截断回 32 位 (溢出 wrap),
                // 否则 64 位寄存器高位垃圾会泄漏给后续移位/除法等运算.
                //
                // `Mul`/`Shl` 是 64 位运算（`imul r64` / `shl r64`），结果必须截断；
                // `Add`/`Sub` 的 32 位无符号形态由 `binop` 的 `use_32bit` 发的是
                // **32 位 ALU**，写目的时已经零扩展 —— 再补一条 `mov dst32,dst32`
                // 是纯冗余（lvm.c 的 `pc += 4` 类每处都白发一条）。
                {
                    let res_ty = e.ty();
                    if res_ty.size() == 4
                        && res_ty.is_int()
                        && matches!(*op, BinOp::Mul | BinOp::Shl)
                    {
                        if Self::is_signed_int(&res_ty) {
                            // movsxd dst, dst32 (符号扩展, 自反 ModRM)
                            use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                            use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                            let gas = format!(
                                "movsxd {}, {}",
                                GAS_REG32[(dst & 15) as usize],
                                GAS_REG64[(dst & 15) as usize]
                            );
                            self.emit_insn(
                                &Insn::mov_ext(true, Width::B32, Width::B64, dst, dst)
                                    .with_gas(gas),
                            )
                            .expect("typed-opcode: mov_ext");
                        } else {
                            // mov dst32, dst32 (零扩展到 64 位)
                            use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                            let gas = format!(
                                "mov {}, {}",
                                GAS_REG32[(dst & 15) as usize],
                                GAS_REG32[(dst & 15) as usize]
                            );
                            let _ = self
                                .emit_insn(&Insn::mov_rm_reg(Width::B32, dst, dst).with_gas(gas));
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
                                // and dst, imm32 (REX.W 81 /4 id)
                                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(
                                    0b100,
                                    dst,
                                    mask as i64,
                                ))
                                .expect("typed-opcode: alu_rm_reg_imm32_b64");
                            } else {
                                let tmp = if dst != 1 { 1u8 } else { 2u8 };
                                // movabs tmp, imm64
                                self.emit_insn(&Insn::movabs_reg_imm64(tmp, mask))
                                    .expect("typed-opcode: movabs_reg_imm64");
                                // and dst, tmp
                                self.emit_insn(&Insn::alu_rm_reg_reg(
                                    AluOp::And,
                                    Width::B64,
                                    dst,
                                    tmp,
                                ))
                                .expect("typed-opcode: alu_rm_reg_reg");
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
                    self.emit_insn(&Insn::call_rel32()); // call rel32
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
                // 形态匹配失败会导致 store 静默丢失 (KASAN 下 *p = v 空操作)。
                // 剥法本身住在 `ast::peel_instrumented_lvalue`（帧规划期按形状判断
                // lvalue 时用同一个），这里的循环多做的只是逐个发出 check 调用。
                let mut lv: &Expr = l.as_ref();
                while let Expr::Comma {
                    l: check, r: inner, ..
                } = lv
                {
                    self.gen_expr(check, Self::EVAL_BASE);
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
                    self.gen_expr(r, Self::EVAL_BASE); // RHS → 向量临时槽 0 (或 rax = 聚合缓冲区地址)
                    if rax_is_agg_addr {
                        self.push(0);
                    }
                    if self.gen_lvalue_addr(lv, Self::EVAL_BASE) {
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                rip_relative: false,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    7,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                            },
                            &[],
                        )); // mov rdi, rax
                        if rax_is_agg_addr {
                            self.pop(6); // rsi = 聚合缓冲区地址
                        } else {
                            let toff = self.vector_temp_base(0, assign_l_ty.size());
                            self.emit_lea_r13(6, toff); // rsi = 临时槽
                        }
                        self.mov_imm(1, assign_l_ty.size() as i128);
                        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
                        self.emit_insn(&Insn::noarg(
                            crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                        )); // rep movsb
                        return;
                    }
                    if rax_is_agg_addr {
                        self.pop(6); // LHS 地址不可得, 落回下方通用路径: 保持栈平衡
                    }
                }
                // 聚合 rvalue → **运行时地址**目标 (成员 / 下标 / 解引用):
                // 与上面 Vector 分支同一条轴 —— 该类型的值装不进一个标量寄存器,
                // 必须整块拷贝。旧分发只把「具名变量」当聚合赋值目标, 这类 LHS
                // 会落到下方恒 8 字节的标量 store, 而聚合 rvalue 的值模型是
                // **地址** (sret 缓冲 / 物化槽), 于是只有第一字被写 (且写的是
                // 源地址), 其余字保留栈上残值 —— `s.f = g()` 掉第二 quad、
                // 协程 awaiter 帧槽 >8B 失效皆此根因。
                // 目标地址先存进一个帧槽, 由 `AggDst::Indirect` 解释; 源侧的
                // 全部物化契约 (Call/sret/va_arg/CompoundLit/整体拷贝) 仍由
                // `gen_aggregate_assign_to` 单一实现承担, 这里不重写任何判据。
                //
                // 复合字面量右值即使目标聚合 <=8B 也必须进本路:
                // `gen_expr(CompoundLit)` 对小结构同样按约定把**临时结构地址**
                // 交付进 rax (而非打包值), 落到下方 Index/成员/解引用的标量
                // int-store 会把这个地址当 8 字节写进目标 (pr79351:
                // `states[j] = (struct state){0}` 后 states[].k 变地址字节)。
                // 该兜底与 Ident 左值路径 (下方 `|| CompoundLit`) 同源同判据。
                if !matches!(lv, Expr::Ident { .. })
                    && (Self::value_is_whole_copy(&assign_l_ty)
                        || (Self::is_aggregate_shape(&assign_l_ty)
                            && matches!(r.as_ref(), Expr::CompoundLit { .. })))
                    && self.gen_lvalue_addr(lv, Self::EVAL_BASE)
                {
                    let addr_slot = self.alloc_sret_slot(8);
                    self.store_stack(0, addr_slot); // [slot] = 目标地址
                    let agg_dst = AggDst::Indirect { addr_slot };
                    self.gen_aggregate_assign_to(&agg_dst, &assign_l_ty, r);
                    // 赋值表达式的值 = 左值转换后的值: 聚合按地址交付
                    self.load_stack(dst, addr_slot);
                    return;
                }
                // _Complex lvalue 赋值 (非 Ident): 统一走物化+分量拷贝。
                // 下方 Index/Deref/Member 的标量 store 会把 rax 恒 8 字节
                // 直接写入: 标量 RHS 污染虚部 (a[i] = -1 得 (-1,-1) 而非
                // (-1,0), pr56837), complex RHS 则把 result_off 物化地址
                // 当数据写入。gen_lvalue_addr 失败时落回旧分支。
                if matches!(assign_l_ty, Type::Complex(_))
                    && !matches!(lv, Expr::Ident { .. })
                    && self.gen_lvalue_addr(lv, Self::EVAL_BASE)
                {
                    self.push(0); // 保存目标地址 (gen_expr 可能占用 rcx)
                    self.gen_complex_store_rcx(r, &assign_l_ty);
                    return;
                }
                if let Expr::Ident { name, ty: l_ty, .. } = lv {
                    // 段地址槽登记（唯一登记点，理由见 `Codegen::local_addr_seg`）：
                    // `-O2` 的 SSA 降把「对象本体带段限定的全局」的访问拆成
                    // `t = &gv; *t = v;` 两条语句，`t` 的指针类型不带地址空间，
                    // 到下面解引用那一笔时表达式里已查不到段 —— 只能在这条
                    // 「左值是单个标识符」的赋值上把段跟着值记下来。
                    // 本行不改任何字节输出，只更新一张表。
                    self.track_addr_seg_assign(&name.name, r);
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
                        // 直写家（`DCC_DEST_ASSIGN=1`，默认关）：目标是有 GP 家的局部、且
                        // 本赋值表达式的值没人要（`dead_here`，由 `Stmt::Expr` 语境置位）时，
                        // 把右值直接算进那个家。今天的路径是「算进 %rax → `mov 家,%eax`」，
                        // 后者每条赋值多一条指令；实测 `luaV_execute` 每步必经的
                        // `i = *(pc++)` 就是这一形状（`mov eax,[rax]` 之后再一条 `mov r12d,eax`）。
                        //
                        // 条件收到最窄，三条都是必需的：
                        //  * 两侧都得是 ≤8B 整型/指针：浮点走 xmm0/st(0)、int128 走 rdx:rax
                        //    成对，那些通道不接受任意 dst；
                        //  * `dead_here`：值还要用时，调用方等的仍是 `dst`，不能只往家里写；
                        //  * volatile 局部不进：它的读/写次数本身就是语义。
                        let r_ty0 = r.ty();
                        let direct_home: Option<u8> = if dest_assign_enabled()
                            && dead_here
                            && Self::stage_scalar_ty(l_ty)
                            && Self::stage_scalar_ty(&r_ty0)
                            && !self.volatile_locals.contains_key(&name.name)
                        {
                            match self.home_for(&name.name) {
                                crate::location::Location::Reg(rid)
                                    if rid.bank == crate::location::RegBank::Gp && rid.n != dst =>
                                {
                                    Some(rid.n)
                                },
                                _ => None,
                            }
                        } else {
                            None
                        };
                        let wdst = direct_home.unwrap_or(dst);
                        self.gen_expr_decay(r, wdst); // 数组衰减: int *p = arr
                                                      // Implicit float→int conversion when RHS is
                                                      // float but LHS is integer (e.g. `a = 0.0;`).
                        let r_ty = r.ty();
                        if matches!(
                            &r_ty,
                            Type::Float | Type::Double | Type::LongDouble | Type::Float128
                        ) && Self::is_integer_type(l_ty)
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
                        if matches!(l_ty, Type::Float | Type::Double | Type::Float128)
                            && Self::is_integer_type(&r_ty)
                        {
                            self.emit_scalar_to_xmm0_if_needed(r, l_ty);
                        }
                        // 赋值表达式的值是左值转换后的值 (C11 6.5.16p3),
                        // 而非右值原值: `ul = us = -1` 中内层 us=-1 的结果须
                        // 先截断到 unsigned short (65535) 再传播给外层, 否则
                        // dst 中未截断的 64 位 -1 直接作为内层表达式的值传出,
                        // ul 得到 0xFFFFFFFFFFFFFFFF.
                        // 归约由 `reduce_assign_value_to_lvalue` 统一实现（下标 /
                        // 解引用 / 成员 / 单次求值复合赋值四条内存左值路径共用同一
                        // 判据，那些路径原先连归约都没有：`long v = (ui[0] =
                        // 0x100000005ULL)` 实测 gcc 5 / dcc 4294967301）。
                        let r_ty = r.ty();
                        self.reduce_assign_value_to_lvalue(wdst, l_ty);
                        if matches!(l_ty, Type::Int128 | Type::UInt128) {
                            // RHS 非 __int128 时, rdx 未设置, 需零扩展/符号扩展
                            let r_ty_for_128 = r.ty();
                            if !matches!(r_ty_for_128, Type::Int128 | Type::UInt128) {
                                // 扩展方向**只看源类型有无符号** (C 转换规则):
                                // 有符号源 (含负常量) 转成更宽的 __int128 必须符号
                                // 扩展; 无符号源才零扩展。旧写法把
                                // `l_ty == UInt128` 也并进零扩展条件, 于是
                                // `unsigned __int128 x = <负 long long>` 被零扩展
                                // (高位丢成 0), pr78416 即此。
                                if matches!(
                                    r_ty_for_128,
                                    Type::Bool
                                        | Type::UChar
                                        | Type::UShort
                                        | Type::UInt
                                        | Type::ULong
                                        | Type::ULongLong
                                ) {
                                    // P11: xor rdx, rdx via typed opcode.
                                    let insn = Insn::new(
                                        X86_64Opcode::AluRm {
                                            op: AluOp::Xor,
                                            width: Width::B64,
                                            dst: OpcodeOperand::Reg(2, Width::B64),
                                            src: OpcodeOperand::Reg(2, Width::B64),
                                        },
                                        &[],
                                    );
                                    if !self.emit_insn(&insn) {
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    2,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    2,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                            },
                                            &[],
                                        )); // xor rdx, rdx
                                    }
                                } else {
                                    self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    )); // cqo (sign-extend rax→rdx:rax)
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
                            } else if matches!(
                                l_ty,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) || matches!(l_ty, Type::Imaginary(_))
                            {
                                // 浮点全局: 值在通道 (xmm0 / st0), 按目标宽度存储
                                // binary128: 整数 RHS 已由下方 856 行的隐式转换统一归一到
                                // xmm0；这里只补「浮点 RHS」（double/float/long double 直接
                                // 赋给 binary128）的情形——整数走 dispatch 层，避免双重
                                // `__floatditf`（实测 `fv2 = ivin` 被转换两次，第二次把
                                // 第一次结果的低 64 位当 long long 再转，得到垃圾 binary128）。
                                if matches!(l_ty, Type::Float128)
                                    && matches!(r_ty, Type::Float | Type::Double | Type::LongDouble)
                                {
                                    self.emit_to_float128(&r_ty);
                                }
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
                        } else if matches!(
                            l_ty,
                            Type::Float | Type::Double | Type::LongDouble | Type::Float128
                        ) || matches!(l_ty, Type::Imaginary(_))
                        {
                            // 局部 float/double 标量落槽前，需把 RHS 送达对应信道：
                            //  * Float/Double → xmm0 (SSE)，由 gen_float_store 内部负责；
                            //  * LongDouble  → x87 st(0)（float_in_st0）。整数/浮点 RHS
                            //    必须在此转换，否则 gen_float_store 的 `fstp tbyte` 会把
                            //    st(0) 上的栈上残留写进变量，导致 `F a; a=(signed char)0;`
                            //    得到 -nan。
                            if self.float_in_st0(l_ty) {
                                self.emit_operand_to_st0(&r_ty, 0);
                            } else if matches!(l_ty, Type::Float128)
                                && matches!(r_ty, Type::Float | Type::Double | Type::LongDouble)
                            {
                                // 整数 RHS 已由 dispatch 856 行统一归一；这里只补浮点
                                // RHS（避免双重 __floatditf，同全局路径）。
                                self.emit_to_float128(&r_ty);
                            }
                            self.gen_float_store(&name.name, l_ty);
                        } else {
                            self.mov_to_stack(wdst, &name.name);
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

                    // 描述符自描述多维下标链 (`ByteOffset::Runtime.terms` 逐项
                    // 携带自己的下标表达式), 旧的「嵌套运行时索引」浅层试探已
                    // 删除: 无法表达的形状由 materialize_address 返回 false,
                    // 落到下面旧路径。
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
                        } else if matches!(
                            elem_ty,
                            Type::Float | Type::Double | Type::LongDouble | Type::Float128
                        ) {
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
                                self.gen_expr(r, Self::EVAL_BASE); // rax = val
                                self.pop(1); // rcx = addr
                                self.emit_seg_for(lv);
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
                                // 阶段2：右值只会写 rax 时不必押栈（同一个判据、同一个配对）。
                                let addr_kept = self.save_addr_for_rhs(lv, r, elem_ty);
                                self.gen_expr(r, Self::EVAL_BASE); // rax = val
                                self.convert_assign_rhs_in_reg(0, elem_ty, &r.ty());
                                self.restore_addr_after_rhs(addr_kept); // rcx = addr
                                self.emit_store_scalar_int_at(
                                    es as usize,
                                    1,
                                    0,
                                    self.lvalue_seg_prefix(lv),
                                );
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
                        //
                        // 先算下标、后算基址（阶段2）：基址这一路 `emit_ident_base_addr`
                        // 只会写 `rax`（局部是 `lea [r13+off]`、全局是 RIP-relative LEA、
                        // VLA 是 `load_stack(dst, off)`），所以不必再把基址押到栈上去躲
                        // `gen_expr(idx, 1)`。C11 里 `a` 与 `i` 的求值顺序本就是 unsequenced，
                        // 交换两者不改变可观察行为；基址不是 Ident 数组/指针的情况走下面
                        // 那条通用路径（基址计算可能自己就要用 rcx，保留押栈形态）。
                        self.gen_expr(idx, 1); // rcx = index
                        self.emit_ident_base_addr(&arr_name.name, arr_ty, 0); // rax = 基址
                        self.emit_index_scale(es, arr, elem_ty);
                        // P11: add rax, rcx via typed opcode (addr in rax).
                        {
                            let insn = Insn::new(
                                X86_64Opcode::AluRm {
                                    op: AluOp::Add,
                                    width: Width::B64,
                                    dst: OpcodeOperand::Reg(0, Width::B64),
                                    src: OpcodeOperand::Reg(1, Width::B64),
                                },
                                &[],
                            );
                            if !self.emit_insn(&insn) {
                                self.emit_insn(&Insn::alu_rm_reg_reg(
                                    crate::codegen::x86_64::opcode::AluOp::Add,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    0,
                                    1,
                                )); // add rax, rcx (addr in rax)
                            }
                        }
                        // 阶段2：同一个 `save_addr_for_rhs` 判据（复杂元素/聚合子路不命中，
                        // 依旧走 push/pop）。
                        let addr_kept = self.save_addr_for_rhs(lv, r, elem_ty);
                        let is_cx_float = matches!(elem_ty, Type::Complex(ref c)
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble));
                        if is_cx_float {
                            if let Type::Complex(ref cinner) = elem_ty {
                                self.store_complex_at_rcx(r, &cinner.base);
                            }
                        } else {
                            self.gen_expr(r, Self::EVAL_BASE); // rax = val
                            self.convert_assign_rhs_in_reg(0, elem_ty, &r.ty());
                            self.restore_addr_after_rhs(addr_kept); // rcx = addr
                            if is_agg_elem {
                                self.gen_agg_elem_store(r, elem_ty, es, dst);
                                return;
                            }
                            if matches!(
                                elem_ty,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) {
                                self.emit_seg_for(lv);
                                self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1);
                            } else {
                                self.emit_store_scalar_int_at(
                                    es as usize,
                                    1,
                                    0,
                                    self.lvalue_seg_prefix(lv),
                                );
                            }
                        }
                    } else {
                        // 通用路径: 基址来自任意 lvalue/指针表达式 (如 s.i[k] = v)
                        let arr_ty = arr.ty();
                        if matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _)) {
                            if !self.gen_lvalue_addr(arr, Self::EVAL_BASE) {
                                self.gen_expr(arr, Self::EVAL_BASE);
                            }
                        } else {
                            self.gen_expr(arr, Self::EVAL_BASE);
                        }
                        self.push(0);
                        self.gen_expr(idx, 1);
                        self.pop(0);
                        self.emit_index_scale(es, arr, elem_ty);
                        // P11: add rax, rcx via typed opcode (addr in rax).
                        {
                            let insn = Insn::new(
                                X86_64Opcode::AluRm {
                                    op: AluOp::Add,
                                    width: Width::B64,
                                    dst: OpcodeOperand::Reg(0, Width::B64),
                                    src: OpcodeOperand::Reg(1, Width::B64),
                                },
                                &[],
                            );
                            if !self.emit_insn(&insn) {
                                self.emit_insn(&Insn::alu_rm_reg_reg(
                                    crate::codegen::x86_64::opcode::AluOp::Add,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    0,
                                    1,
                                )); // add rax, rcx (addr in rax)
                            }
                        }
                        // 阶段2：上面同一对助手（通用下标回退路径）。
                        let addr_kept2 = self.save_addr_for_rhs(lv, r, elem_ty);
                        let is_cx_float2 = matches!(elem_ty, Type::Complex(ref c)
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble));
                        if is_cx_float2 {
                            if let Type::Complex(ref cinner) = elem_ty {
                                self.store_complex_at_rcx(r, &cinner.base);
                            }
                        } else {
                            self.gen_expr(r, Self::EVAL_BASE);
                            self.convert_assign_rhs_in_reg(0, elem_ty, &r.ty());
                            self.restore_addr_after_rhs(addr_kept2);
                            if is_agg_elem {
                                self.gen_agg_elem_store(r, elem_ty, es, dst);
                                return;
                            }
                            if matches!(
                                elem_ty,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) {
                                self.emit_seg_for(lv);
                                self.emit_store_scalar_float_at(&r.ty(), elem_ty, 1);
                            } else {
                                self.emit_store_scalar_int_at(
                                    es as usize,
                                    1,
                                    0,
                                    self.lvalue_seg_prefix(lv),
                                );
                            }
                        }
                    }
                } else if let Expr::Deref { e: ptr_e, .. } = lv {
                    // *ptr = val
                    // 段前缀沿基址链下潜取（`*pp` 这类「指针的指针」外层是普通指针、
                    // 里层才带段时，只看一层会漏发前缀）。
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
                            self.gen_expr(r, Self::EVAL_BASE);
                            self.mov(6, 0); // rsi = rax
                        } else if rhs_is_call {
                            // Call returning aggregate: gen_expr leaves
                            // rax = &sret buffer (or &temp for reg return).
                            // Use rax directly as source address.
                            self.gen_expr(r, Self::EVAL_BASE);
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
                            self.gen_lvalue_addr(r, Self::EVAL_BASE); // rax = src addr
                            self.mov(6, 0); // rsi = rax
                        } else {
                            // General: gen_expr, store to temp
                            self.gen_expr(r, Self::EVAL_BASE);
                            let temp_off = self.var_offset("<agg_deref_tmp>");
                            self.store_stack(0, temp_off);
                            if agg_sz > 8 {
                                self.store_stack(2, temp_off + 8);
                            }
                            self.emit_lea_r13(6, temp_off);
                        }
                        // 2. Get destination address (pointer value) into rdi
                        self.gen_expr(ptr_e, Self::EVAL_BASE); // rax = ptr value
                        self.mov(7, 0); // rdi = rax
                                        // 3. 拷贝到段限定目标
                                        //
                                        // 带段时**不能**用 `rep movsb`：64 位模式下字符串指令的段前缀被 CPU
                                        // 忽略，挂一枚 `65` 的 rep movsb 会写到非段内存（旧实现把前缀发在
                                        // `cld` 前面，那是给 cld 加的，等于什么都没做）。带段路径改走
                                        // `copy_agg_to_seg`（常量尺寸）/ `copy_bytes_to_seg_runtime`（VLA）。
                        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
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
                            None => {
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                ))
                                .expect("typed-opcode: noarg");
                            }, // rep movsb
                            Some(seg) if assign_l_ty.has_vla() => {
                                self.copy_bytes_to_seg_runtime(seg)
                            },
                            Some(seg) => self.copy_agg_to_seg(agg_sz, seg),
                        }
                    } else {
                        self.gen_expr(ptr_e, Self::EVAL_BASE); // rax = address
                                                               // 阶段2：右值是只会写 rax 的形态（字面量/标量名字）时，地址一条
                                                               // `mov rcx, rax` 就到位，不必再押栈；否则保持今天的 push/pop 形态。
                        let addr_kept = self.save_addr_for_rhs(lv, r, &assign_l_ty);
                        let is_cx_ptr = matches!(assign_l_ty, Type::Complex(ref c)
                            if matches!(&c.base,
                                Type::Float | Type::Double | Type::LongDouble));
                        if is_cx_ptr {
                            if let Some(seg) = seg_prefix {
                                self.emit_insn(&Insn::seg_prefix(seg));
                            }
                            if let Type::Complex(ref cinner) = assign_l_ty {
                                self.store_complex_at_rcx(r, &cinner.base);
                            }
                        } else {
                            self.gen_expr(r, Self::EVAL_BASE); // rax = val
                            self.convert_assign_rhs_in_reg(0, &assign_l_ty, &r.ty());
                            self.restore_addr_after_rhs(addr_kept); // rcx = addr
                            if matches!(
                                &assign_l_ty,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) {
                                // __seg_gs/__seg_fs 指针存储: 发射段前缀 (%gs:/%fs:)
                                if let Some(seg) = seg_prefix {
                                    self.emit_insn(&Insn::seg_prefix(seg));
                                }
                                self.emit_store_scalar_float_at(&r.ty(), &assign_l_ty, 1);
                            } else {
                                // 按 *ptr 类型宽度 store (窄类型只写低 n 字节 = 截断
                                // 语义)。旧实现两处皆错: 恒用 8 字节 movq 会越界覆盖
                                // 相邻内存 (decNumber *up=(Unit)out 破坏后续单元 →
                                // 系数全 0); 而 16 字节又只发低半那一笔 —— `__int128`
                                // 丢高半 (long-mul-128.c 的 `*first = h1` 实测存成
                                // `0xC << 64`), `_Float128` 掉进本分支被当 `__int128`
                                // 从 rax:rdx 存 (rax/rdx 是残值, 实测 `*p = v` 得 0)。
                                // 判据与字节发射单源于 `emit_store_scalar_int_at` /
                                // `emit_store_scalar_float_at`, 这里只做分发。
                                self.emit_store_scalar_int_at(assign_l_ty.size(), 1, 0, seg_prefix);
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
                        self.gen_expr(expr, Self::EVAL_BASE); // rax = 指针值
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
                                use crate::codegen::x86_64::emit::gas::mem_disp as gas_mem_disp;
                                let gas = format!("lea {}, %rax", gas_mem_disp(13, off));
                                let mut bytes = vec![0x49, 0x8D];
                                if off.abs() < 128 {
                                    bytes.push(0x45);
                                    bytes.push(off as i8 as u8);
                                } else {
                                    bytes.push(0x85);
                                    bytes.extend_from_slice(&(off as i32).to_le_bytes());
                                }
                                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                            }
                        }
                    } else if !self.gen_lvalue_addr(expr, Self::EVAL_BASE) {
                        // 通用路径失败: 退回直接求值 (旧行为)
                        self.gen_expr(expr, Self::EVAL_BASE);
                    }
                    // 3. 加上字段偏移 (超过 disp32 的超大结构体字段偏移走
                    //    add_reg_byte_offset 的 movabs+add 路径, 见 instructions.rs)
                    if let Some(ref f) = field_info {
                        let field_off = f.offset as i64;
                        if field_off != 0 {
                            if Self::fits_disp32(field_off) {
                                let gas = format!("add ${}, %rax", field_off);
                                self.emit_insn(&Insn::alu_acc_imm32_b64_acc_gas(
                                    0x05,
                                    field_off as i32,
                                    &gas,
                                ));
                            } else {
                                self.add_reg_byte_offset(0, field_off);
                            }
                        }
                    }
                    if is_bitfield {
                        // 位域写入: 读-改-写 (使用帧内临时槽, 按嵌套深度动态分配)
                        let f = field_info.as_ref().unwrap();
                        let bw = f.bit_width.unwrap();
                        // 容器宽度取真实存储单元（与读取路径 `bitfield_unit_size` 同
                        // 一真源），写回才能不抹掉同单元邻位域。
                        let container_size = f.bitfield_unit_size();
                        let bit_off = f.bit_offset;
                        // 按嵌套深度计算帧槽偏移 (避免嵌套位域 RMW 互相覆盖).
                        // 走 LayoutMap::bf_rmw_slot 取 BF_RMW 专用带, 不再从 -8/-16
                        // 硬算 (原写法与金丝雀 -8、callee-saved -16..-40 同格,
                        // depth>=8 时 i8 回绕成 +120(%r13) 写调用者帧/返回地址).
                        // slot 0 = 容器地址, slot 1 = 容器值. 统一为 3 槽/层,
                        // 与自增路径 (increment.rs) 几何一致, 消灭 H1b 同格.
                        let depth = self.bf_rmw_depth as i64;
                        let addr_off = self.frame_layout.bf_rmw_slot(depth, 0);
                        let val_off = self.frame_layout.bf_rmw_slot(depth, 1);
                        // rax = 容器地址
                        // 保存地址到帧临时槽1: [r13 + addr_off]
                        // mov [r13+addr_off], rax (r13=13)
                        self.emit_insn(&Insn::mov_rm_mem(false, Width::B64, 0, 13, addr_off));
                        // 加载当前容器值 (结果统一放入 rax)
                        match container_size {
                            4 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B32,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B32,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(0),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                    },
                                    &[],
                                ))
                                .expect("typed-opcode: new");
                            }, // mov eax, [rax] (零扩展)
                            8 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(0),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                    },
                                    &[],
                                ))
                                .expect("typed-opcode: new");
                            }, // mov rax, [rax]
                            2 => {
                                // mov ax, [rax]; movzx rax, ax
                                let _ =
                                    self.emit_insn(&Insn::mov_rm_mem(true, Width::B16, 0, 0, 0));
                                self.emit_insn(&Insn::mov_ext(false, Width::B16, Width::B64, 0, 0));
                            },
                            1 => {
                                // movzx rax, byte [rax]
                                self.emit_insn(&Insn::mov_ext_mem(
                                    false,
                                    Width::B8,
                                    Width::B64,
                                    0,
                                    0,
                                    0,
                                ))
                                .expect("typed-opcode: mov_ext_mem");
                            },
                            _ => {},
                        }
                        // 保存容器值到帧临时槽2: [r13 + val_off]
                        self.emit_insn(&Insn::mov_rm_mem(false, Width::B64, 0, 13, val_off));
                        // 计算右值 (递增嵌套深度, 防止内部位域 RMW 覆盖本层帧槽)
                        self.bf_rmw_depth += 1;
                        self.gen_expr(r, Self::EVAL_BASE); // rax = 新值
                        self.bf_rmw_depth -= 1;
                        // 掩码: and rax, (1<<bw)-1
                        if bw < 64 {
                            let mask: u64 = (1u64 << bw) - 1;
                            if mask <= 0xFFFFFFFF {
                                // and eax, imm32 (REX.W 81 /4 id, 但 REX.W 不影响 imm32 形式；
                                // 实际 x86-64: 48 25 id = and rax, sign-extended imm32 — 与 alu_rm_reg_imm32_b64 一致)
                                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0b100, 0, mask as i64))
                                    .expect("typed-opcode: alu_rm_reg_imm32_b64");
                            } else {
                                // >32 位掩码: movabs rdx, mask; and rax, rdx
                                self.emit_insn(&Insn::movabs_reg_imm64(2, mask))
                                    .expect("typed-opcode: movabs_reg_imm64");
                                self.emit_insn(&Insn::alu_rm_reg_reg(AluOp::And, Width::B64, 0, 2))
                                    .expect("typed-opcode: alu_rm_reg_reg");
                            }
                        }
                        // 左移到位置: shl rax, bit_off
                        if bit_off > 0 {
                            // 步骤 2d: 收敛到 Shift{Shl, B64, rax, Imm8(bit_off)}.
                            // 原手写: 48 C1 E0 <bit_off>.
                            let insn = Insn::new(
                                X86_64Opcode::Shift {
                                    op: ShiftOp::Shl,
                                    width: Width::B64,
                                    dst: OpcodeOperand::Reg(0, Width::B64),
                                    amount: ShiftAmount::Imm8(bit_off as u8),
                                },
                                &[],
                            );
                            if !self.emit_insn(&insn) {
                                self.emit_insn(&Insn::shift_reg_imm8_c1_b64(4, 0, bit_off as u8));
                            }
                        }
                        // rax = 移位后的新值
                        // 重新从内存加载容器值 (RHS 求值可能已修改内存中的容器)
                        // rcx = [r13 + addr_off] (地址)
                        self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 1, 13, addr_off)); // mov rcx, [r13+addr_off]
                                                                                              // rcx = [rcx] (从内存重新加载容器)
                        match container_size {
                            4 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B32,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            1,
                                            crate::codegen::shared::asm_operand::Width::B32,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(1),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                    },
                                    &[],
                                ))
                                .expect("typed-opcode: new");
                            }, // mov ecx, [rcx]
                            8 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            1,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(1),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                    },
                                    &[],
                                ))
                                .expect("typed-opcode: new");
                            }, // mov rcx, [rcx]
                            2 => {
                                // movzx rcx, word [rcx]
                                self.emit_insn(&Insn::mov_ext_mem(
                                    false,
                                    Width::B16,
                                    Width::B64,
                                    1,
                                    1,
                                    0,
                                ))
                                .expect("typed-opcode: mov_ext_mem");
                            },
                            1 => {
                                // movzx rcx, byte [rcx]
                                self.emit_insn(&Insn::mov_ext_mem(
                                    false,
                                    Width::B8,
                                    Width::B64,
                                    1,
                                    1,
                                    0,
                                ))
                                .expect("typed-opcode: mov_ext_mem");
                            },
                            _ => {},
                        }
                        // 清除目标位: inv_mask = ~(((1<<bw)-1) << bit_off)
                        let mask: u64 = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
                        let inv_mask = !(mask << bit_off);
                        // and rcx, inv_mask
                        if (inv_mask as u32 as i32 as i64 as u64) == inv_mask {
                            // 可以用 imm32 符号扩展
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 1, inv_mask as i64));
                        // and rcx, imm32
                        } else {
                            // >32 位: movabs rdx, inv_mask; and rcx, rdx
                            self.emit_insn(&Insn::movabs_reg_imm64(2, inv_mask))
                                .expect("typed-opcode: movabs_reg_imm64");
                            let _ =
                                self.emit_insn(&Insn::alu_rm_reg_reg(AluOp::And, Width::B64, 1, 2));
                        }
                        // or rcx, rax
                        self.emit_insn(&Insn::alu_rm_reg_reg(AluOp::Or, Width::B64, 1, 0))
                            .expect("typed-opcode: alu_rm_reg_reg");
                        // 加载地址: rdx = [r13 + addr_off]
                        self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 2, 13, addr_off)); // mov rdx, [r13+addr_off]
                                                                                              // 存储容器 (结果在 rcx 中)
                        match container_size {
                            4 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B32,
                                        dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(2),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                                            1,
                                            crate::codegen::shared::asm_operand::Width::B32,
                                        ),
                                    },
                                    &[],
                                ))
                                .expect("typed-opcode: new");
                            }, // mov [rdx], ecx
                            8 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(2),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                                            1,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                    },
                                    &[],
                                ))
                                .expect("typed-opcode: new");
                            }, // mov [rdx], rcx
                            2 => {
                                let _ =
                                    self.emit_insn(&Insn::mov_rm_mem(false, Width::B16, 1, 2, 0));
                            }, // mov [rdx], cx
                            1 => {
                                let _ =
                                    self.emit_insn(&Insn::mov_rm_mem(false, Width::B8, 1, 2, 0));
                            }, // mov [rdx], cl
                            _ => {},
                        }
                        // 位域赋值表达式的结果 = 截断并符号扩展后的值 (C11 6.5.16p3)
                        // 从内存重新加载容器，提取位域值并符号扩展
                        {
                            let f = field_info.as_ref().unwrap();
                            let is_signed = Self::is_signed_int(&f.ty);
                            // rdx = [r13 + addr_off] (地址)
                            self.emit_insn(&Insn::mov_rm_mem(true, Width::B64, 2, 13, addr_off)); // mov rdx, [r13+addr_off]
                                                                                                  // rdx = [rdx] (重新加载容器)
                            match container_size {
                                4 => {
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B32,
                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                2,
                                                crate::codegen::shared::asm_operand::Width::B32,
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Mem(
                                                crate::codegen::x86_64::opcode::MemOperand {
                                                    disp: 0,
                                                    base: Some(2),
                                                    index: None,
                                                    seg: None,
                                                },
                                            ),
                                        },
                                        &[],
                                    ))
                                    .expect("typed-opcode: new");
                                }, // mov edx, [rdx]
                                8 => {
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                2,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Mem(
                                                crate::codegen::x86_64::opcode::MemOperand {
                                                    disp: 0,
                                                    base: Some(2),
                                                    index: None,
                                                    seg: None,
                                                },
                                            ),
                                        },
                                        &[],
                                    ))
                                    .expect("typed-opcode: new");
                                }, // mov rdx, [rdx]
                                2 => {
                                    // movzx rdx, word [rdx]
                                    self.emit_insn(&Insn::mov_ext_mem(
                                        false,
                                        Width::B16,
                                        Width::B64,
                                        2,
                                        2,
                                        0,
                                    ))
                                    .expect("typed-opcode: mov_ext_mem");
                                },
                                1 => {
                                    // movzx rdx, byte [rdx]
                                    self.emit_insn(&Insn::mov_ext_mem(
                                        false,
                                        Width::B8,
                                        Width::B64,
                                        2,
                                        2,
                                        0,
                                    ))
                                    .expect("typed-opcode: mov_ext_mem");
                                },
                                _ => {},
                            }
                            // shr rdx, bit_off
                            if bit_off > 0 {
                                // 步骤 2d: Shift{Shr, B64, rdx, Imm8(bit_off)}.
                                let insn = Insn::new(
                                    X86_64Opcode::Shift {
                                        op: ShiftOp::Shr,
                                        width: Width::B64,
                                        dst: OpcodeOperand::Reg(2, Width::B64),
                                        amount: ShiftAmount::Imm8(bit_off as u8),
                                    },
                                    &[],
                                );
                                if !self.emit_insn(&insn) {
                                    self.emit_insn(&Insn::shift_reg_imm8_c1_b64(
                                        5,
                                        2,
                                        bit_off as u8,
                                    ));
                                }
                            }
                            // mask to bw bits
                            if bw < 64 {
                                let mask: u64 = (1u64 << bw) - 1;
                                if mask <= 0xFFFFFFFF {
                                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 2, mask as i64));
                                // and rdx, imm32
                                } else {
                                    // >32 位: movabs rax, mask; and rdx, rax
                                    self.emit_insn(&Insn::movabs_reg_imm64(0, mask))
                                        .expect("typed-opcode: movabs_reg_imm64");
                                    self.emit_insn(&Insn::alu_rm_reg_reg(
                                        AluOp::And,
                                        Width::B64,
                                        2,
                                        0,
                                    ))
                                    .expect("typed-opcode: alu_rm_reg_reg");
                                }
                            }
                            // sign-extend if signed bitfield
                            if is_signed && bw < 64 {
                                let shift = 64 - bw;
                                // shl rdx, shift
                                // 步骤 2d: Shift{Shl|Sar, B64, rdx, Imm8(shift)}.
                                let insn_shl = Insn::new(
                                    X86_64Opcode::Shift {
                                        op: ShiftOp::Shl,
                                        width: Width::B64,
                                        dst: OpcodeOperand::Reg(2, Width::B64),
                                        amount: ShiftAmount::Imm8(shift as u8),
                                    },
                                    &[],
                                );
                                if !self.emit_insn(&insn_shl) {
                                    self.emit_insn(&Insn::shift_reg_imm8_c1_b64(4, 2, shift as u8));
                                }
                                // sar rdx, shift
                                let insn_sar = Insn::new(
                                    X86_64Opcode::Shift {
                                        op: ShiftOp::Sar,
                                        width: Width::B64,
                                        dst: OpcodeOperand::Reg(2, Width::B64),
                                        amount: ShiftAmount::Imm8(shift as u8),
                                    },
                                    &[],
                                );
                                if !self.emit_insn(&insn_sar) {
                                    self.emit_insn(&Insn::shift_reg_imm8_c1_b64(7, 2, shift as u8));
                                }
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
                            // ⚠ 这三条形状判据与规划期的
                            // `func_setup::agg_member_scratch_used` 是同一事实的两面：改任何
                            // 一边都必须同步另一边，否则就是「预算算少 → E1133」那一类假硬错。
                            let rhs_is_lvalue = matches!(
                                &**r,
                                Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
                            );
                            let rhs_is_ident = matches!(&**r, Expr::Ident { .. });
                            let rhs_is_compound = matches!(&**r, Expr::CompoundLit { .. });
                            self.push(0); // save target address
                            if rhs_is_lvalue {
                                // Lvalue RHS: get source address directly
                                self.gen_lvalue_addr(r, Self::EVAL_BASE); // rax = source address
                                self.pop(1); // rcx = target address
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            6,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                    },
                                    &[],
                                )); // mov rsi, rax
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
                                self.gen_expr(r, Self::EVAL_BASE); // rax = compound lit temp address
                                self.pop(1); // rcx = target address
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            6,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                    },
                                    &[],
                                )); // mov rsi, rax
                            } else {
                                // General RHS: evaluate, store rax:rdx to temp
                                self.gen_expr(r, Self::EVAL_BASE);
                                self.pop(1); // rcx = target address
                                             // 发射器自己的暂存对象：它属于「发射期才登记的晚分配」那一族
                                             // （与 `__compound_lit_*` / `__vmsize_*` 同口径），所以在本发射点
                                             // **显式** `register_var` 并给出真实类型，而不是去碰 `var_offset` 的
                                             // 规划命中路径。尺寸是 16 字节而不是 8：下面 `agg_sz > 8` 时还要写
                                             // `temp_off + 8`（rdx），旧写法按 `Type::Int` 懒注册一个槽，第二半就
                                             // 落在相邻对象的下半字节上。
                                let temp_off = self.register_var("<agg_member_tmp>", &Type::Int128);
                                self.store_stack(0, temp_off);
                                if agg_sz > 8 {
                                    self.store_stack(2, temp_off + 8);
                                }
                                self.emit_lea_r13(6, temp_off); // rsi = source
                            }
                            self.emit_insn(&Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                    rip_relative: false,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        7,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        1,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            )); // mov rdi, rcx (dst)
                            self.emit_insn(&Insn::noarg(
                                crate::codegen::x86_64::opcode::NoArgOp::Cld,
                            )); // cld
                            self.mov_imm(1, agg_sz as i128); // rcx = size (rep movsb counter)
                            self.emit_insn(&Insn::noarg(
                                crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                            )); // rep movsb
                        } else {
                            // 阶段2：同上，同一个 `save_addr_for_rhs` 判据。
                            let addr_kept = self.save_addr_for_rhs(lv, r, &assign_l_ty);
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
                                self.gen_expr(r, Self::EVAL_BASE); // rax = 值 (浮点: xmm0)
                                self.convert_assign_rhs_in_reg(0, &assign_l_ty, &r.ty());
                                self.restore_addr_after_rhs(addr_kept); // rcx = 目标地址
                                if let Some(fty) = &fty {
                                    if let Type::Complex(ref cinner) = fty {
                                        // 整数 complex 字段: rvalue 已由外层
                                        // gen_expr 物化在 result_off (rax=地址
                                        // 约定), 整体拷贝 isz*2 字节到 [rcx]
                                        let isz = cinner.base.size();
                                        let result_off = self.complex_result_off();
                                        self.emit_lea_r13(6, result_off); // rsi
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                                rip_relative: false,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    7,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    1,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                            },
                                            &[],
                                        )); // mov rdi, rcx
                                        self.mov_imm(1, (isz * 2) as i128);
                                        self.emit_insn(&Insn::noarg(
                                            crate::codegen::x86_64::opcode::NoArgOp::Cld,
                                        ));
                                        self.emit_insn(&Insn::noarg(
                                            crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                        )); // cld; rep movsb
                                    } else if matches!(
                                        fty,
                                        Type::Float
                                            | Type::Double
                                            | Type::LongDouble
                                            | Type::Float128
                                    ) {
                                        self.emit_seg(self.lvalue_seg_prefix(lv));
                                        self.emit_store_scalar_float_at(&r.ty(), fty, 1);
                                    // 单一真源：收浮影子副本
                                    } else {
                                        self.emit_store_scalar_int_at(
                                            size,
                                            1,
                                            0,
                                            self.lvalue_seg_prefix(lv),
                                        );
                                    }
                                } else {
                                    self.emit_store_scalar_int_at(
                                        size,
                                        1,
                                        0,
                                        self.lvalue_seg_prefix(lv),
                                    );
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
                    if self.gen_lvalue_addr(ce, Self::EVAL_BASE) {
                        // 阶段2：分量类型是整数/指针且右值只会写 rax 时不必押栈；
                        // 浮点/long double 分量的值通道是 xmm0/st(0)，白名单本来不接它们。
                        let addr_kept = self.save_addr_for_rhs(lv, r, &inner);
                        self.gen_expr(r, Self::EVAL_BASE); // 值 (浮点: xmm0)
                        self.restore_addr_after_rhs(addr_kept); // rcx = 目标地址
                        if matches!(inner, Type::LongDouble) {
                            // LongDouble 分量: 把 RHS 统一搬到 st0 (80-bit
                            // tbyte), 再 fstpt 直写 [rcx+part_off]。必须先
                            // 调用 emit_operand_to_st0: RHS 可能是整数
                            // (如 x+1, 值在 rax) 或 SSE float/double (值在
                            // xmm0), 而不是已在 st0 的 long double 表达式。
                            self.emit_operand_to_st0(&r.ty(), 0);
                            self.emit_x87_fstp_at(1, part_off as i32, &inner);
                        } else if is_float_inner {
                            self.emit_scalar_to_xmm0_if_needed(r, &inner);
                            let swidth = if matches!(inner, Type::Float) {
                                SimdWidth::SS
                            } else {
                                SimdWidth::SD
                            };
                            self.emit_insn(&Insn::simd_mov_mem_base(
                                true,
                                swidth,
                                0,
                                1,
                                part_off as i32,
                            )); // movss/movsd [rcx+disp], xmm0
                        } else {
                            // 整数分量: 值在 rax(:rdx), 按内型宽度存储
                            if part_off != 0 {
                                self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, 1, part_off as i64));
                                // add rcx, imm32
                            }
                            self.emit_store_scalar_int_at(inner.size(), 1, 0, None);
                        }
                    } else {
                        // 非 lvalue (如 __imag__ (a+b)): 不可赋值, 仅求值右操作数
                        self.gen_expr(r, Self::EVAL_BASE);
                    }
                } else {
                    // 通用 lvalue 回退: 用于 Cast-lvalue 或未来
                    // 新增的 lvalue 形式 (如复合表达式解引用).
                    // 通过 gen_lvalue_addr 计算目标地址, 再按类型
                    // 宽度发射 store 指令. 避免未匹配任何分支时
                    // 赋值静默变成 no-op.
                    let assign_sz = assign_l_ty.size();
                    let is_float_ty = matches!(
                        assign_l_ty,
                        Type::Float | Type::Double | Type::LongDouble | Type::Float128
                    );
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
                                if self.gen_lvalue_addr(lv, Self::EVAL_BASE) {
                                    self.push(0); // 保存目标地址 (gen_expr 破坏 rax)
                                    self.gen_expr(r, Self::EVAL_BASE); // xmm0 = 实部
                                    self.pop(1); // rcx = 目标地址
                                    self.emit_scalar_to_xmm0_if_needed(r, &inner.base);
                                    let inner_sz = inner.size() as i64;
                                    let swidth = if matches!(inner.base, Type::Float) {
                                        crate::codegen::x86_64::opcode::SimdWidth::SS
                                    } else {
                                        crate::codegen::x86_64::opcode::SimdWidth::SD
                                    };
                                    // 实部 → [rcx]
                                    self.emit_insn(&Insn::simd_mov_mem_base(true, swidth, 0, 1, 0));
                                    // 虚部 → xmm0 → [rcx+inner_sz]
                                    self.gen_complex_imag_into_xmm0(r, &inner.base);
                                    self.emit_insn(&Insn::simd_mov_mem_base(
                                        true,
                                        swidth,
                                        0,
                                        1,
                                        inner_sz as i32,
                                    ));
                                    return;
                                } else {
                                    // 无法取地址: 仅求值 RHS
                                    self.gen_expr(r, Self::EVAL_BASE);
                                    return;
                                }
                            }
                        }
                    }
                    if is_aggregate {
                        // 聚合类型 (>8B): 取地址 + rep movsb 拷贝
                        if self.gen_lvalue_addr(lv, Self::EVAL_BASE) {
                            self.push(0); // 保存目标地址
                            self.gen_expr(r, Self::EVAL_BASE);
                            // 源地址: gen_expr 对聚合类型结果在帧临时区
                            let src_off = self.complex_result_off();
                            self.emit_lea_r13(6, src_off); // rsi = src
                            self.pop(7); // rdi = dst
                            self.emit_insn(&Insn::noarg(
                                crate::codegen::x86_64::opcode::NoArgOp::Cld,
                            )); // cld
                            self.mov_imm(2, assign_sz as i128); // rcx = size
                            self.emit_insn(&Insn::noarg(
                                crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                            )); // rep movsb
                        } else {
                            self.gen_expr(r, Self::EVAL_BASE);
                        }
                    } else {
                        // 标量写：地址与右值挤在同一个号上，三条路按代价从低到高
                        //（判据单源 `plan_addr_keep`）：
                        //   * `InRcx`：地址直接进 store 要的 `rcx`，右值只写 `rax`；
                        //   * `Late`：右值不改地址链上的名字，算完右值再一条 `lea` 重算
                        //     （C11 6.5.16 两个操作数求值无序）——push/pop 整对消失；
                        //   * `OnStack`：其余形态照旧押栈。
                        //
                        // 地址只算一次：`keep` 决定落哪个号，三条臂共用同一个
                        // `gen_lvalue_addr` 入口 —— 写成两个 `else if` 串联会在「白名单命中但
                        // 取址失败」时把同一段地址计算发两遍。
                        let keep = self.plan_addr_keep(lv, r, &assign_l_ty);
                        let addr_dst = if matches!(keep, AddrKeep::InRcx) {
                            1
                        } else {
                            Self::EVAL_BASE
                        };
                        if self.gen_lvalue_addr(lv, addr_dst) {
                            match keep {
                                // 地址已经在 rcx，什么都不用发。
                                AddrKeep::InRcx => {},
                                // 地址之后重算，这一份就着右值一起作废。
                                AddrKeep::Late(_) => {},
                                AddrKeep::OnStack => self.push(0), // 保存目标地址
                            }
                            self.gen_expr(r, Self::EVAL_BASE); // rax = val (浮点: xmm0)
                                                               // 值没人要 ⇒ 只做到存之前必要的那一段转换，不再把 rax 归约到
                                                               // 左值类型（store 自己就按左值宽度截断）。寄存器家里的赋值走另外
                                                               // 那条臂，那里寄存器内容就是变量的表示，必须保持规范形态。
                            if dead_here {
                                self.convert_assign_rhs_in_reg_for_store(0, &assign_l_ty, &r.ty());
                            } else {
                                self.convert_assign_rhs_in_reg(0, &assign_l_ty, &r.ty());
                            }
                            self.restore_addr_after_rhs(keep); // rcx = 目标地址
                            if is_float_ty {
                                self.emit_store_scalar_float_at(&r.ty(), &assign_l_ty, 1);
                            // 单一真源：收浮影子副本
                            } else {
                                self.emit_store_scalar_int_at(assign_sz, 1, 0, None);
                            }
                        } else {
                            // gen_lvalue_addr 失败: 无法取地址, 仅求值 RHS
                            self.gen_expr(r, Self::EVAL_BASE);
                        }
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
                    self.gen_expr(e, Self::EVAL_BASE);
                    match op {
                        UnOp::Neg => {
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                                crate::codegen::shared::asm_operand::Width::B64,
                                0,
                            )); // neg rax
                                // P11: adc rdx, 0 via AluRm typed (imm=0 fits i8).
                            let insn_adc = Insn::new(
                                X86_64Opcode::AluRm {
                                    op: AluOp::Adc,
                                    width: Width::B64,
                                    dst: OpcodeOperand::Reg(2, Width::B64),
                                    src: OpcodeOperand::Imm(0),
                                },
                                &[],
                            );
                            if !self.emit_insn(&insn_adc) {
                                self.emit_insn(&Insn::raw_bytes(&[0x48, 0x83, 0xD2, 0x00]));
                                // adc rdx, 0
                            }
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                                crate::codegen::shared::asm_operand::Width::B64,
                                2,
                            )); // neg rdx
                        },
                        UnOp::BitNot => {
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Not,
                                crate::codegen::shared::asm_operand::Width::B64,
                                0,
                            )); // not rax
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Not,
                                crate::codegen::shared::asm_operand::Width::B64,
                                2,
                            )); // not rdx
                        },
                        UnOp::Not => {
                            // P11: or rax, rdx via typed opcode.
                            {
                                let insn = Insn::new(
                                    X86_64Opcode::AluRm {
                                        op: AluOp::Or,
                                        width: Width::B64,
                                        dst: OpcodeOperand::Reg(0, Width::B64),
                                        src: OpcodeOperand::Reg(2, Width::B64),
                                    },
                                    &[],
                                );
                                if !self.emit_insn(&insn) {
                                    self.emit_insn(&Insn::alu_rm_reg_reg(
                                        crate::codegen::x86_64::opcode::AluOp::Or,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        0,
                                        2,
                                    )); // or rax, rdx
                                }
                            }
                            self.emit_insn(&Insn::test_rm_reg(
                                crate::codegen::shared::asm_operand::Width::B64,
                                0,
                                0,
                            )); // test rax, rax
                            self.emit_insn(&Insn::setcc_reg(
                                crate::codegen::x86_64::opcode::CondCode::Eq,
                                0,
                            )); // sete al
                            self.emit_insn(
                                &Insn::mov_ext(false, Width::B8, Width::B64, 0, 0)
                                    .with_gas("movzbq %al, %rax".to_string()),
                            );
                            // P11: xor rdx, rdx via typed opcode (high = 0).
                            {
                                let insn = Insn::new(
                                    X86_64Opcode::AluRm {
                                        op: AluOp::Xor,
                                        width: Width::B64,
                                        dst: OpcodeOperand::Reg(2, Width::B64),
                                        src: OpcodeOperand::Reg(2, Width::B64),
                                    },
                                    &[],
                                );
                                if !self.emit_insn(&insn) {
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                2,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                2,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                        },
                                        &[],
                                    )); // high = 0
                                }
                            }
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
                    && (matches!(
                        e.ty(),
                        Type::Double | Type::Float | Type::LongDouble | Type::Float128
                    ) || matches!(e.ty(), Type::Imaginary(_)))
                {
                    self.gen_expr(e, Self::EVAL_BASE); // 值按类型落在 st(0) 或 xmm0
                    let e_ty = e.ty();
                    let neg_ty = if let Type::Imaginary(ref inner) = e_ty {
                        &inner.base
                    } else {
                        &e_ty
                    };
                    // 单一判据：st(0) 通道用 `fchs`（翻转 80-bit 的符号位，
                    // 不经 xmm0、不丢尾数）；否则才用 xorpd 翻符号位掩码。
                    // `neg_ty` 已是去掉 Imaginary 包装后的内型：`_Imaginary long
                    // double` 的内型是 LongDouble ⇒ float_in_st0 为真 ⇒ fchs。旧实现
                    // 在此叠加 `!matches!(e_ty, Imaginary(_))` 把 imaginary 整体排除出
                    // st(0) 通道, 于是 ild 走 xorpd: fldt 已把值压进 st(0), 却对陈旧
                    // xmm0 翻符号位, st(0) 从不 fchs、函数直接 ret ⇒ 值错 + x87 栈
                    // 泄漏。删除该例外: imaginary 的内型判词与实浮点完全同口径。
                    if self.float_in_st0(neg_ty) {
                        self.emit_x87_chs();
                    } else if matches!(neg_ty, Type::Float128) {
                        // binary128: 符号位在**高** 64 位的最高位 (位 127)，
                        // 与 double 不同 ⇒ 走专门的 xorpd 掩码路径。
                        self.emit_float128_neg();
                    } else {
                        match neg_ty {
                            Type::Float => {
                                // 翻转 32 位符号位: movabs rax, 0x8000_0000
                                //   66 48 0f 6e c8 movq xmm1, rax
                                //   0f 57 c1       xorps xmm0, xmm1
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Imm(
                                            0x80000000u64 as i64,
                                        ),
                                    },
                                    &[],
                                ));
                                self.emit_insn(&Insn::gpr_xmm(
                                    true,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    0,
                                    1,
                                ));
                                self.emit_insn(&Insn::simd_arith_reg(
                                    SimdArithOp::Xorps,
                                    SimdWidth::PS,
                                    0,
                                    1,
                                ));
                            },
                            _ => {
                                // 翻转 64 位符号位（double，以及 -mno-sse 下
                                // 担心遗漏的兼容分支已由上方 st(0) 接走）
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Imm(
                                            0x8000000000000000u64 as i64,
                                        ),
                                    },
                                    &[],
                                ));
                                self.emit_insn(&Insn::gpr_xmm(
                                    true,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    0,
                                    1,
                                ));
                                self.emit_insn(&Insn::simd_arith_reg(
                                    SimdArithOp::Xorpd,
                                    SimdWidth::PD,
                                    0,
                                    1,
                                ));
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
                    let result_off = self.complex_result_off();
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
                    if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                        // lvalue (变量/字段/下标): 用 rcx 保存地址 (flip_sign_xmm
                        // 会破坏 rax 但不动 rcx), 逐分量加载/flip/store
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                rip_relative: false,
                                width: crate::codegen::shared::asm_operand::Width::B64,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    1,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                    0,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                ),
                            },
                            &[],
                        )); // mov rcx, rax
                        let imag_off = inner_sz; // 变量内存虚部槽 (连续布局)
                        let swidth = if matches!(inner, Type::Float) {
                            SimdWidth::SS
                        } else {
                            SimdWidth::SD
                        };
                        // 实部: [rcx] → xmm0 → flip → [result_off]
                        self.emit_insn(&Insn::simd_mov_mem_base(false, swidth, 0, 1, 0)); // movss/movsd xmm0, [rcx]
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                        // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                        self.emit_insn(&Insn::simd_mov_mem_base(
                            false,
                            swidth,
                            0,
                            1,
                            imag_off as i32,
                        ));
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
                    } else {
                        // rvalue: gen_expr 后 xmm0=实部
                        self.gen_expr(e, Self::EVAL_BASE);
                        if matches!(e.as_ref(), Expr::Call { .. }) {
                            // SSE 返回: 虚部在 xmm1, 物化到临时区
                            self.emit_insn(&Insn::raw_bytes(&[pfx, 0x41, 0x0F, 0x11]));
                            if (result_off + inner_sz).abs() < 128 {
                                self.emit_insn(&Insn::raw_bytes(&[
                                    0x4D,
                                    (result_off + inner_sz) as u8,
                                ]));
                            } else {
                                self.emit_insn(&Insn::raw_bytes(&[0x8D]));
                                self.emit_insn(&Insn::raw_bytes(
                                    &((result_off + inner_sz) as i32).to_le_bytes(),
                                ));
                            }
                        }
                        // 实部翻转符号位并写回临时区
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                        // 虚部翻转符号位并写回临时区
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off + inner_sz);
                        self.flip_sign_xmm(0, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off + inner_sz);
                    }
                    // 重载实部到 xmm0 (complex rvalue 约定: xmm0=实部)
                    self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
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
                    let result_off = self.complex_result_off();
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
                        if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                            self.emit_insn(&Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                    rip_relative: false,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        1,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        0,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            )); // mov rcx, rax
                            let imag_off = inner_sz;
                            let swidth = if matches!(inner, Type::Float) {
                                SimdWidth::SS
                            } else {
                                SimdWidth::SD
                            };
                            // 实部: [rcx] → xmm0 → [result_off] (不变)
                            self.emit_insn(&Insn::simd_mov_mem_base(false, swidth, 0, 1, 0)); // movss/movsd xmm0, [rcx]
                            self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                            // 虚部: [rcx+off] → xmm0 → flip → [result_off+inner_sz]
                            self.emit_insn(&Insn::simd_mov_mem_base(
                                false,
                                swidth,
                                0,
                                1,
                                imag_off as i32,
                            ));
                            self.flip_sign_xmm(0, &inner);
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x41, 0x0F, 0x11],
                                result_off + inner_sz,
                            );
                        } else {
                            self.gen_expr(e, Self::EVAL_BASE);
                            // 实部已经在 xmm0, 直接写回
                            self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                            // 虚部: 加载并翻转
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x41, 0x0F, 0x10],
                                result_off + inner_sz,
                            );
                            self.flip_sign_xmm(0, &inner);
                            self.emit_xmm0_r13_disp(
                                &[pfx, 0x41, 0x0F, 0x11],
                                result_off + inner_sz,
                            );
                        }
                        // 重载实部到 xmm0
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
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
                    if self.gen_lvalue_addr(e, Self::EVAL_BASE) {
                        self.mov(6, 0); // rsi = &src
                        self.emit_lea_r13(7, result_off); // rdi = r13+result_off
                        self.mov_imm(1, (inner_sz * 2) as i128);
                        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld));
                        self.emit_insn(&Insn::noarg(
                            crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                        )); // cld; rep movsb
                    } else {
                        self.gen_expr(e, Self::EVAL_BASE);
                        // rvalue 已按统一约定物化到 result_off (rax=地址);
                        // 保险起见若 rax 不指向 result_off 则重拷
                        // (整数 complex rvalue 均遵守此约定)
                    }
                    // 2. 虚部取负 (按内型宽度)
                    match inner_sz {
                        8 => {
                            self.load_stack(0, result_off + 8);
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                                crate::codegen::shared::asm_operand::Width::B64,
                                0,
                            )); // neg rax
                            self.store_stack(0, result_off + 8);
                        },
                        4 => {
                            self.load_stack(0, result_off + 4);
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                                crate::codegen::shared::asm_operand::Width::B32,
                                0,
                            )); // neg eax
                            self.emit_store(0, result_off + 4, 4);
                        },
                        2 => {
                            self.load_stack(0, result_off + 2);
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                                crate::codegen::shared::asm_operand::Width::B16,
                                0,
                            )); // neg ax
                            self.emit_store(0, result_off + 2, 2);
                        },
                        _ => {
                            self.load_stack(0, result_off + 1);
                            self.emit_insn(&Insn::unary_rm_reg(
                                crate::codegen::x86_64::opcode::UnaryOp::Neg,
                                crate::codegen::shared::asm_operand::Width::B8,
                                0,
                            )); // neg al
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
                        //
                        // 类型判据必须取**整数提升后**的操作数类型: 操作数是
                        // char/short (含 unsigned) 时 C11 6.3.1.1 先提升为 int,
                        // 结果是**有符号** int。直接用操作数类型会把 `unsigned char`
                        // 的取负判成无符号 ⇒ 发 32 位 neg 且只零扩展, 高 32 位留 0
                        // (pr107541: `-a / 8` 读到 536870880 而非 -31; `long l = -a`
                        // 得 4294967042 而非 -254)。__int128 (16B) 提升后仍是自身,
                        // 判据与旧行为一致。
                        let promo_ty = e.ty().int_promotion();
                        let ty_size = promo_ty.size();
                        let is_signed = Self::is_signed_int(&promo_ty);
                        let use_32bit_neg = ty_size == 4 && is_signed && self.trapv;
                        if ty_size >= 8 || (ty_size >= 4 && is_signed && !use_32bit_neg) {
                            // 64-bit neg: REX.W neg
                            let _ =
                                self.emit_insn(&Insn::unary_rm_reg(UnaryOp::Neg, Width::B64, dst));
                        } else if use_32bit_neg {
                            // 32-bit neg for trapv: 检测 32 位溢出
                            let _ =
                                self.emit_insn(&Insn::unary_rm_reg(UnaryOp::Neg, Width::B32, dst));
                        } else {
                            // 32 位无符号: REX 前缀无 .W
                            let _ =
                                self.emit_insn(&Insn::unary_rm_reg(UnaryOp::Neg, Width::B32, dst));
                        }
                        // -ftrapv: 有符号取负溢出检查 (e.g. -INT_MIN)
                        // neg 在操作数为类型最小值时置 OF=1
                        // 有符号性同样按提升后的类型判: `unsigned char` 取负在 C 里
                        // 是 int 运算, 溢出检查不该被跳过 (见上方 promo_ty 注释)。
                        if self.trapv && Self::is_signed_int(&promo_ty) {
                            let trap_label = self.fresh_label();
                            let ok = self.fresh_label();
                            self.jmp_rel32(&trap_label, JumpKind::Jo);
                            self.jmp_rel32(&ok, JumpKind::Jmp);
                            self.label(&trap_label);
                            self.emit_insn(&Insn::call_rel32()); // call rel32
                            let call_at = self.code.len() as u64 - 4;
                            self.pending_func_rels
                                .push((call_at, "__dcc_trapv_overflow".to_string()));
                            self.label(&ok);
                            // 32 位 trapv neg 后符号扩展: 32 位 neg 结果在 eax,
                            // 需要 movslq 到 rax 保持 dcc 的 64 位寄存器约定
                            if promo_ty.size() == 4 {
                                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                                let gas = format!(
                                    "movsxd {}, {}",
                                    GAS_REG32[(dst & 15) as usize],
                                    GAS_REG64[(dst & 15) as usize]
                                );
                                self.emit_insn(
                                    &Insn::mov_ext(true, Width::B32, Width::B64, dst, dst)
                                        .with_gas(gas),
                                )
                                .expect("typed-opcode: mov_ext");
                            }
                        }
                    },
                    UnOp::Not => {
                        // 逻辑非: !x = (x == 0) ? 1 : 0
                        // test dst, dst (reflexive B64)
                        let test_reg =
                            crate::codegen::x86_64::abi::regnames::att_name_of_num(dst, 64)
                                .unwrap_or_else(|| "%rax".to_string());
                        let test_gas = format!("test {}, {}", test_reg, test_reg);
                        let _ = self
                            .emit_insn(&Insn::test_rm_reg(Width::B64, dst, dst).with_gas(test_gas));
                        // sete al: 零 → 1, 非零 → 0 (0F 94 C0)
                        self.emit_insn(&Insn::setcc_reg(
                            crate::codegen::x86_64::opcode::CondCode::Eq,
                            0,
                        ));
                        // movzx dst, al (0F B6 /r: reg=dst, r/m=al=0)
                        let movzbq_gas = format!("movzbq %al, {}", test_reg);
                        self.emit_insn(
                            &Insn::mov_ext(false, Width::B8, Width::B64, dst, 0)
                                .with_gas(movzbq_gas),
                        )
                        .expect("typed-opcode: mov_ext");
                    },
                    UnOp::BitNot => {
                        self.emit_insn(&Insn::unary_rm_reg(UnaryOp::Not, Width::B64, dst))
                            .expect("typed-opcode: unary_rm_reg");
                        // not flips all 64 bits. The result must be
                        // truncated to the expression type width, then
                        // re-extended. For types < int, C integer
                        // promotion means the NOT result is at least
                        // 32 bits wide.
                        //
                        // 判据取**整数提升后**的操作数类型 (单一真源 int_promotion):
                        // `~a` (a 为 unsigned char) 的结果类型是**有符号** int,
                        // 必须 movsxd 回 64 位。旧写法直接读操作数类型, 把无符号
                        // 小类型判成无符号 ⇒ 只零扩展, 高 32 位留 0 (pr107541:
                        // `~a / 8` 得 536870880 而非 -31; `long l = ~a` 得
                        // 4294967041 而非 -255)。
                        let result_ty = e.ty().int_promotion();
                        let result_sz = result_ty.size();
                        let effective_sz = result_sz.max(4); // int promotion
                        if effective_sz <= 4 {
                            // Result fits in 32 bits: truncate to 32
                            // bits then zero/sign-extend to 64.
                            let is_signed = Self::is_signed_int(&result_ty);
                            if is_signed {
                                // movsxd dst, dst (sign-extend 32→64)
                                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                                let gas = format!(
                                    "movsxd {}, {}",
                                    GAS_REG32[(dst & 15) as usize],
                                    GAS_REG64[(dst & 15) as usize]
                                );
                                self.emit_insn(
                                    &Insn::mov_ext(true, Width::B32, Width::B64, dst, dst)
                                        .with_gas(gas),
                                )
                                .expect("typed-opcode: mov_ext");
                            } else {
                                // mov dst32, dst32 (zero-extend 32→64)
                                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                                let gas = format!(
                                    "mov {}, {}",
                                    GAS_REG32[(dst & 15) as usize],
                                    GAS_REG32[(dst & 15) as usize]
                                );
                                self.emit_insn(
                                    &Insn::mov_rm_reg(Width::B32, dst, dst).with_gas(gas),
                                )
                                .expect("typed-opcode: mov_rm_reg");
                            }
                        } else if result_sz < 8 {
                            unreachable!()
                        }
                        // GCC 位域算术语义: ~ 的操作数若是位域, 结果需掩码到字段
                        // 宽度 (而非声明类型宽度). 例: unsigned long i:33, ~i 在
                        // 33 位精度内取反 → 0, 而非 64 位全反. 仅对 33..63 位宽
                        // 生效 (≤32 位由上方整数提升路径处理, ==64 无需掩码),
                        // 与二元运算的位域精度处理保持一致.
                        if let Some(bf_width) = Self::expr_bitfield_width(e) {
                            if bf_width < 64 && bf_width > 32 {
                                let mask: u64 = (1u64 << bf_width) - 1;
                                if mask <= 0x7FFFFFFF {
                                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(
                                        0b100,
                                        dst,
                                        mask as i64,
                                    ))
                                    .expect("typed-opcode: alu_rm_reg_imm32_b64");
                                } else {
                                    let tmp = if dst != 1 { 1u8 } else { 2u8 };
                                    self.emit_insn(&Insn::movabs_reg_imm64(tmp, mask))
                                        .expect("typed-opcode: movabs_reg_imm64");
                                    self.emit_insn(&Insn::alu_rm_reg_reg(
                                        AluOp::And,
                                        Width::B64,
                                        dst,
                                        tmp,
                                    ))
                                    .expect("typed-opcode: alu_rm_reg_reg");
                                }
                            }
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
                                let insn = Insn::new(
                                    X86_64Opcode::MovExt {
                                        rip_relative: false,
                                        sign: true,
                                        from: crate::codegen::shared::asm_operand::Width::B32,
                                        to: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: OpcodeOperand::Reg(
                                            1,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: OpcodeOperand::Reg(
                                            1,
                                            crate::codegen::shared::asm_operand::Width::B32,
                                        ),
                                    },
                                    &[],
                                );
                                self.emit_insn(&insn);
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
                                        // 步骤 2d: Shift{Shl, B64, rcx(=1), Imm8(shift)}.
                                        let insn = Insn::new(
                                            X86_64Opcode::Shift {
                                                op: ShiftOp::Shl,
                                                width: Width::B64,
                                                dst: OpcodeOperand::Reg(1, Width::B64),
                                                amount: ShiftAmount::Imm8(shift),
                                            },
                                            &[],
                                        );
                                        if !self.emit_insn(&insn) {
                                            self.emit_insn(&Insn::shift_reg_imm8_c1_b64(
                                                4, 1, shift,
                                            ));
                                        }
                                    } else {
                                        self.imul_imm(1, stride as i128); // imul rcx, rcx, stride
                                    }
                                }
                            }
                            // dst += rcx
                            // P11: add rcx, dst via typed AluRm (AluRm r/m=dst, reg=rcx).
                            self.emit_insn(&Insn::alu_rm_reg_reg(AluOp::Add, Width::B64, dst, 1));
                            // 下降到元素类型
                            cur_ty = elem.base.clone();
                        }
                    }
                }
            },
            Expr::Comma { l, r, .. } => {
                // 逗号运算符: 求值 lhs 丢弃, 求值 rhs 结果放入 dst
                self.gen_expr(l, Self::EVAL_BASE);
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
                // 嵌套函数内 &&父标签: 走 resolve_nested, 与 Stmt::Goto 保持一致
                let label_name = self.c_label_to_asm_resolve_nested(&label.name);
                // P11: LEA dst, [rip+disp32] via typed Lea RIP-relative (encoder 字节等价).
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::Lea {
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            dst,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        mem: crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: None,
                            index: None,
                            seg: None,
                        },
                        rip_relative: true,
                    },
                    &[],
                ));
                let disp32_pos = self.pos() - 4;
                self.label_addr_refs.push((disp32_pos, label_name));
            },
            Expr::StmtExpr { block, .. } => {
                // 语句表达式 ({...}): 执行块内语句, 最后表达式值留在 dst
                let items = &block.items;
                let cleanup_base = self.begin_cleanup_scope();
                // static local 的可见性也是块作用域的。这里是 `gen_block_items`
                // 在语句表达式上的那份副本, 两端必须与它同步。
                self.push_static_scope();
                let last_idx = items.len().saturating_sub(1);
                for (i, item) in items.iter().enumerate() {
                    match item {
                        BlockItem::Stmt(s) if i == last_idx => {
                            // 最后一条表达式语句: 结果放入 dst。标签外壳先剥掉
                            // 并照常发射 —— `({ skip: g(s); })` 的值就是被标签的
                            // `g(s)`，旧实现只认裸 `Stmt::Expr`，带标签的最后一条因此
                            // 落到下面「当普通语句发」那条丢弃结果的路径，dst 里剩下
                            // 上一个操作数的残值（§11.13 的 N6）。
                            match self.emit_tail_labels(s) {
                                Stmt::Expr { e, .. } => self.gen_stmt_expr_value(e, dst),
                                Stmt::Return { val: Some(e), .. } => self.gen_expr(e, dst),
                                other => self.gen_stmt(other),
                            }
                        },
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            // 绑在两条 `continue` 之前 (同 `gen_block_item`)。
                            self.bind_static_local(v);
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
                                    if let Expr::InitList { inits, .. } = init {
                                        if let Type::Array(elem_ty, _) = &v.ty {
                                            let elem_size = elem_ty.size();
                                            let mut off: i32 = 0;
                                            for item in inits.iter() {
                                                if let InitItem::Expr(e) = item {
                                                    self.gen_expr(e, Self::EVAL_BASE);
                                                    self.emit_global_store_offset(
                                                        0,
                                                        &v.name.name,
                                                        off,
                                                    );
                                                    off += elem_size as i32;
                                                }
                                            }
                                        } else {
                                            self.gen_expr(init, Self::EVAL_BASE);
                                            self.emit_global_store(0, &v.name.name);
                                        }
                                    } else {
                                        self.gen_expr(init, Self::EVAL_BASE);
                                        self.emit_global_store(0, &v.name.name);
                                    }
                                }
                                continue;
                            }
                            // 声明处的家必须走与 `gen_block_item` 同一个入口。这里是那条逻辑的
                            // 一份副本，而副本漏了 `bind_decl_home` 与 `cleanup_fn` 登记：实测
                            // `({ register long j asm("r12"); j = 5; j; })` 里那个绑定完全失效
                            // （读写都走内存，而同一个声明在普通块里则真的走 %r12），而
                            // `__attribute__((cleanup))` 在语句表达式里从不调用。读不了家的后端
                            // 不会报错，只会把变量留在两处都不认的状态里。
                            self.bind_decl_home(v);
                            if let Some(ref fn_name) = v.cleanup_fn {
                                self.cleanup_vars
                                    .push((v.name.name.clone(), fn_name.clone()));
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
                                // 字符数组 + 字符串字面量：复制内容而不是指针 (同 `gen_block_item`)。
                                if let Expr::String { data, .. } = init {
                                    if matches!(&v.ty, Type::Array(_, _)) {
                                        let (name, ty) = (v.name.name.clone(), v.ty.clone());
                                        self.gen_char_array_str_init(&name, &ty, data);
                                    } else {
                                        self.gen_expr(init, Self::EVAL_BASE);
                                        self.mov_to_stack(0, &v.name.name);
                                    }
                                } else if matches!(
                                    // 浮点: 结果在 xmm0, 必须用 movsd/movss 存回栈 (同 Stmt::Decl 路径)
                                    &v.ty,
                                    Type::Float | Type::Double | Type::LongDouble | Type::Float128
                                ) {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    if matches!(&v.ty, Type::Float128) {
                                        self.emit_to_float128(&init.ty());
                                    } else {
                                        self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                                    }
                                    self.gen_float_store(&v.name.name, &v.ty);
                                } else {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    // Implicit float→int conversion when
                                    // initializer is float but variable is
                                    // integer (e.g. `int a = 0.0;`).
                                    let init_ty = init.ty();
                                    if matches!(
                                        &init_ty,
                                        Type::Float
                                            | Type::Double
                                            | Type::LongDouble
                                            | Type::Float128
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
                // 块作用域的 cleanup 必须在这里发出（以前这一臂只有登记、没有收尾，
                // 于是 `({...})` 里的 `__attribute__((cleanup))` 静默失效）。登记了 cleanup 时
                // 先把值在帧里放稳：cleanup 是真 call，会毁掉 caller-saved 的 dst，而跨 call
                // 不得用 push/pop 护住（pr44575：那会把 16 字节对齐弄坏）。
                if self.cleanup_vars.len() > cleanup_base {
                    let slot = self.alloc_sret_slot(8);
                    self.store_stack(dst, slot);
                    self.end_cleanup_scope(cleanup_base);
                    self.load_stack(dst, slot);
                } else {
                    self.end_cleanup_scope(cleanup_base);
                }
                // 语句表达式也是一个块作用域: static 绑定同样要还原。
                self.pop_static_scope();
            },
            Expr::Cast { ty, e, .. } => {
                // 单一入口：本 arm 曾是 `gen_expr_cast` 的逐字副本。此处模式
                // 内层 `e` 遮蔽了 gen_expr 的形参 `e`, 而 gen_expr_cast 要的
                // 正是那个被转换的子表达式, 语义一致。
                self.gen_expr_cast(ty, e, dst);
            },
            Expr::Cond { .. } => {
                // 单一入口: `gen_expr_cond` 与这里曾是逐字副本, 收敛到 helper
                // 后聚合 (struct/union) 支的物化约定只有一处可以改。
                self.gen_expr_cond(e, dst);
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
                // P11: typed Lea RIP-relative (encoder 字节等价).
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::Lea {
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            dst,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        mem: crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: None,
                            index: None,
                            seg: None,
                        },
                        rip_relative: true,
                    },
                    &[],
                ));
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
                        // `&var` 是取地址：走家的单一读入口（寄存器家会是内部错误）
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
                            let off = self.var_offset(&name.name);
                            // 同一件事的第三份手抄本（另两份在 lvalue.rs / mem/addr.rs）：
                            // dst>=8 缺 REX.R 会把 lea 的目的编成低 8 号。单源入口。
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
                            use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                            let gas =
                                format!("add ${}, {}", field_off, GAS_REG64[(dst & 15) as usize]);
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64_full_gas(
                                0, dst, field_off, &gas,
                            ));
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
                            // 局部指针: 加载指针值。走家的单一读入口 —— `var_offset` 只
                            // 认识栈槽，绑了寄存器的指针在这里会读到从未写过的帧槽。
                            self.mov_from_stack(0, &arr_name.name);
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
                    self.emit_insn(
                        &Insn::alu_rm_reg_reg(AluOp::Add, Width::B64, 0, 1)
                            .with_gas("add %rcx, %rax".to_string()),
                    ); // add rax, rcx
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
                        use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                        let gas = format!("add ${}, {}", inner_sz, GAS_REG64[(dst & 15) as usize]);
                        self.emit_insn(&Insn::alu_rm_reg_imm32_b64_full_gas(
                            0,
                            dst,
                            inner_sz as i32,
                            &gas,
                        ));
                    }
                } else {
                    // 兜底：上面几条具名形状之外，也必须把地址算出来。旧写法在这里
                    // **什么都不发**，于是 `%dst`（通常就是 EVAL_BASE=%rax）带着上一条
                    // 指令的残值被当成地址交出去 —— ASan 的 store 检查因此检查一个随机
                    // 地址（实测 `q->head = t` 的 `__asan_store4` 入参没算过）。「静默
                    // 不发音」比报错难查一个量级，这里不允许再出现。
                    if !self.gen_lvalue_addr(e, dst) {
                        self.gen_expr(e, dst);
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
                // 向量成员：rvalue 的值模型是「字节在 `vector_temp_base(0, size)`」，而
                // `gen_expr_member_rvalue` 的字段装载只认标量 / 浮点 / `__int128` 三档：
                // 对 16 字节向量它「匹配不上任何宽度」而不发一个字节（实测
                // `V0 = ((union { V src; W dst; }){…}).dst` 恒得 0），对 ≤8 字节向量则按
                // 标量 GP 装载，位模式进了 dst 而消费端仍按槽 0 读 ⇒ 拿到残值。
                // `gen_vector_load_into_slot` 的成员分支已经会把整个成员按地址整块
                // 拷进槽 0，直接复用它，不在此重写取址与拷贝判据。
                if matches!(ty, Type::Vector(_, _)) {
                    self.gen_vector_load_into_slot(e, 0);
                    let temp_off = self.vector_temp_base(0, ty.size());
                    // 与 `gen_vector_binop` 各出口同形：交付 `rax = &槽 0`，供聚合赋值端
                    // 与 `gen_call` 尾部的 ABI 物化段使用。
                    self.emit_lea_r13(0, temp_off);
                    return;
                }
                // 单一入口：本 arm 曾是 `gen_expr_member_rvalue` 的逐字副本
                // (两份同源代码, 只有这一份在跑), 任何只改 lvalue.rs 的修复
                // 都会静默失效。现只委派。
                self.gen_expr_member_rvalue(expr, field, is_arrow, ty, dst);
            },
            Expr::CompoundLit { .. } => {
                // 单一入口：本 arm 曾是 `gen_expr_compound_lit`（`expr/compound.rs`）的逐字
                // 副本。两份同源只会发生「只改了不在跑的那一份」的静默失效（上面
                // Member / Cast 两个 arm 就是同一族缺陷修完留下的先例），所以这里只委派。
                self.gen_expr_compound_lit(e, dst);
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
                    self.emit_insn(&Insn::ud2()); // ud2
                    self.mov_imm(dst, 0);
                }
            },
            _ => {
                self.expr_warn(e, "codegen unsupported expression; emitted as ud2");
                self.emit_insn(&Insn::ud2()); // ud2
                self.mov_imm(dst, 0);
            },
        }
    }
}
