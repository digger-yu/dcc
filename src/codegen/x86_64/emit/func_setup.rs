// src/codegen/x86_64/emit/func_setup.rs
// Per-function setup helpers: frame bookkeeping, local analysis walks and
// small register/stack helpers used by compile_func. Manually migrated from
// the pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::Codegen;
use std::collections::HashMap;

impl Codegen {
    pub fn reset_reg_tracker(&mut self) {
        self.reg_allocated = 0;
        self.callee_saved_used = 0;
        // file-scope 的 `register asm` 绑定在 compile_func 开头就已经写进
        // reg_bindings, 而上面两行把两个位图清零。不重新 claim 就等于宣告
        // 「本函数不使用这些寄存器」, 序言里对应的保存会被 patch_callee_saves
        // 改成 nop —— 调用者存放在里面的值就此消失 (本地声明那条路径在绑定处
        // 当场 claim, 因为它的时机晚于本函数)。
        let bound: Vec<u8> = self.reg_bindings.values().map(|(r, _)| *r).collect();
        for reg in bound {
            self.claim_bound_register(reg);
        }
    }

    pub fn stack_reserve_bytes(&self) -> i64 {
        // 向量/bitcast 临时槽需要 3 个 16 字节槽: slot0 是
        // vector 结果约定, slot1/2 用于同尺寸重解释 cast。
        let vector_scratch = 48;
        if self.target.red_zone {
            vector_scratch
        } else {
            128 + vector_scratch
        }
    }

    pub fn parse_reg_name(name: &str) -> Option<u8> {
        match name.to_lowercase().as_str() {
            "al" | "ah" | "ax" | "eax" | "rax" | "%rax" | "%eax" | "%ax" => Some(0),
            "bl" | "bh" | "bx" | "ebx" | "rbx" | "%rbx" | "%ebx" | "%bx" => Some(3),
            "cl" | "ch" | "cx" | "ecx" | "rcx" | "%rcx" | "%ecx" | "%cx" => Some(1),
            "dl" | "dh" | "dx" | "edx" | "rdx" | "%rdx" | "%edx" | "%dx" => Some(2),
            "sil" | "si" | "esi" | "rsi" | "%rsi" | "%esi" | "%si" => Some(6),
            "dil" | "di" | "edi" | "rdi" | "%rdi" | "%edi" | "%di" => Some(7),
            "spl" | "sp" | "esp" | "rsp" | "%rsp" | "%esp" | "%sp" => Some(4),
            "bpl" | "bp" | "ebp" | "rbp" | "%rbp" | "%ebp" | "%bp" => Some(5),
            "r8" | "r8d" | "r8w" | "r8b" | "%r8" => Some(8),
            "r9" | "r9d" | "r9w" | "r9b" | "%r9" => Some(9),
            "r10" | "r10d" | "r10w" | "r10b" | "%r10" => Some(10),
            "r11" | "r11d" | "r11w" | "r11b" | "%r11" => Some(11),
            "r12" | "r12d" | "r12w" | "r12b" | "%r12" => Some(12),
            "r13" | "r13d" | "r13w" | "r13b" | "%r13" => Some(13),
            "r14" | "r14d" | "r14w" | "r14b" | "%r14" => Some(14),
            "r15" | "r15d" | "r15w" | "r15b" | "%r15" => Some(15),
            // APX EGPR (Extended General Purpose Registers) r16-r31
            "r16" | "r16d" | "r16w" | "r16b" | "%r16" => Some(16),
            "r17" | "r17d" | "r17w" | "r17b" | "%r17" => Some(17),
            "r18" | "r18d" | "r18w" | "r18b" | "%r18" => Some(18),
            "r19" | "r19d" | "r19w" | "r19b" | "%r19" => Some(19),
            "r20" | "r20d" | "r20w" | "r20b" | "%r20" => Some(20),
            "r21" | "r21d" | "r21w" | "r21b" | "%r21" => Some(21),
            "r22" | "r22d" | "r22w" | "r22b" | "%r22" => Some(22),
            "r23" | "r23d" | "r23w" | "r23b" | "%r23" => Some(23),
            "r24" | "r24d" | "r24w" | "r24b" | "%r24" => Some(24),
            "r25" | "r25d" | "r25w" | "r25b" | "%r25" => Some(25),
            "r26" | "r26d" | "r26w" | "r26b" | "%r26" => Some(26),
            "r27" | "r27d" | "r27w" | "r27b" | "%r27" => Some(27),
            "r28" | "r28d" | "r28w" | "r28b" | "%r28" => Some(28),
            "r29" | "r29d" | "r29w" | "r29b" | "%r29" => Some(29),
            "r30" | "r30d" | "r30w" | "r30b" | "%r30" => Some(30),
            "r31" | "r31d" | "r31w" | "r31b" | "%r31" => Some(31),
            _ => None,
        }
    }

    /// 回填本函数序言/尾声的 callee-saved 保存/恢复指令.
    /// 单遍编译下序言先于函数体发射, 无法预知函数体是否使用这些寄存器;
    /// 函数体生成完毕后 callee_saved_used 已完整, 将未使用寄存器的
    /// 4 字节 mov 改写为 4×0x90 (nop), 消除叶子函数的冗余保存/恢复.
    /// 尾声可能在每个 return 处内联发射, 统一在此处回填, 不依赖生成顺序.
    /// sites_base: compile_func 开头记录的 sites 起始下标 (嵌套函数隔离).
    pub fn patch_callee_saves(&mut self, sites_base: usize) {
        let used = self.callee_saved_used;
        for (pos, bit) in self.callee_save_patch_sites.drain(sites_base..) {
            if used & bit == 0 {
                let p = pos as usize;
                self.code[p..p + 4].fill(0x90);
            }
        }
    }

    pub fn emit_store_rax_r13(&mut self, off: i64) {
        self.store_stack(0, off);
    }

    pub fn ends_with_return(items: &[BlockItem]) -> bool {
        match items.last() {
            Some(BlockItem::Stmt(Stmt::Return { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Break { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Continue { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Block { b, .. })) => Self::ends_with_return(&b.items),
            Some(BlockItem::Stmt(Stmt::If { then_, else_, .. })) => {
                Self::ends_with_return(&[BlockItem::Stmt(*then_.clone())])
                    && else_
                        .as_ref()
                        .is_some_and(|e| Self::ends_with_return(&[BlockItem::Stmt(*e.clone())]))
            },
            _ => false,
        }
    }

    /// Collect all label names defined in a statement tree.
    /// Used for non-local goto detection in nested functions.
    pub fn collect_local_labels_stmt(stmt: &Stmt, out: &mut std::collections::HashSet<String>) {
        match stmt {
            Stmt::Label { name, stmt, .. } => {
                out.insert(name.name.clone());
                Self::collect_local_labels_stmt(stmt, out);
            },
            // C++ 零成本异常: 落地垫标签同样是本函数内定义的标签
            // (`goto` 可以指向它), 必须计入 non-local goto 判定。
            Stmt::EhLandingPad { label, .. } => {
                out.insert(label.name.clone());
            },
            Stmt::Invoke {
                call, landing_pad, ..
            } => {
                // `landing_pad == None` 是 lp=0 的「本帧不接管」区间, 没有标签可登记。
                if let Some(pad) = landing_pad {
                    out.insert(pad.name.clone());
                }
                Self::collect_local_labels_stmt(call, out);
            },
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        Self::collect_local_labels_stmt(s, out);
                    }
                }
            },
            Stmt::If { then_, else_, .. } => {
                Self::collect_local_labels_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_local_labels_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_local_labels_stmt(body, out);
            },
            Stmt::For { body, .. } => Self::collect_local_labels_stmt(body, out),
            Stmt::Switch { cases, .. } => {
                for c in cases {
                    for item in &c.body.items {
                        if let BlockItem::Stmt(s) = item {
                            Self::collect_local_labels_stmt(s, out);
                        }
                    }
                }
            },
            _ => {},
        }
    }

    // 扫描最大表达式嵌套深度 (binop/call 各+1)
    pub fn max_expr_depth_stmt(items: &[BlockItem]) -> u32 {
        items
            .iter()
            .map(|item| match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    v.init.as_ref().map_or(0, Self::max_expr_depth_expr)
                },
                BlockItem::Stmt(s) => Self::max_expr_depth_stmt_inner(s),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn max_expr_depth_stmt_inner(s: &Stmt) -> u32 {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::max_expr_depth_expr(e)
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                let d = Self::max_expr_depth_expr(cond);
                d.max(Self::max_expr_depth_stmt_inner(then_)).max(
                    else_
                        .as_ref()
                        .map_or(0, |e| Self::max_expr_depth_stmt_inner(e)),
                )
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::max_expr_depth_expr(cond).max(Self::max_expr_depth_stmt_inner(body))
            },
            Stmt::For { cond, body, .. } => cond
                .as_ref()
                .map_or(0, Self::max_expr_depth_expr)
                .max(Self::max_expr_depth_stmt_inner(body)),
            Stmt::Block { b, .. } => Self::max_expr_depth_stmt(&b.items),
            _ => 0,
        }
    }

    fn max_expr_depth_expr(e: &Expr) -> u32 {
        match e {
            Expr::Binop { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Call { callee, args, .. } => {
                1 + Self::max_expr_depth_expr(callee).max(
                    args.iter()
                        .map(Self::max_expr_depth_expr)
                        .max()
                        .unwrap_or(0),
                )
            },
            Expr::Comma { l, r, .. } => {
                Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Assign { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Unary { e, .. }
            | Expr::Cast { e, .. }
            | Expr::AddrOf { e, .. }
            | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => Self::max_expr_depth_expr(e),
            Expr::Index { arr, idx, .. } => {
                Self::max_expr_depth_expr(arr).max(Self::max_expr_depth_expr(idx))
            },
            Expr::Member { expr, .. } => Self::max_expr_depth_expr(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => Self::max_expr_depth_expr(cond)
                .max(Self::max_expr_depth_expr(then_))
                .max(Self::max_expr_depth_expr(else_)),
            _ => 0,
        }
    }

    // ========================================================================
    // sret / 物化槽字节水位静态扫描
    // ------------------------------------------------------------------------
    // 与运行时分配器 alloc_sret_slot 完全同口径: 每个槽按 16 字节对齐,
    // 自 sret_region_base 向下累计; 槽在其所属的“语句/块 item”边界死亡。
    // 扫描返回整函数任意执行路径上的最大同时存活字节水位, 供帧规划把
    // 槽区计入 frame_size (旧 bump 不做扫描, 槽位越过帧底覆盖 call
    // outgoing 区, cld 夹具在此段错误)。
    //
    // 建模规则 (必须与代码生成的 save/restore 挂钩点一一对应):
    //   * 一个 BlockItem 从 incoming 水位开始, 结束时回到 incoming —
    //     gen_block_items / 函数顶层循环在每个 item 边界 save/restore;
    //   * 单个表达式树内部不释放 (兄弟子表达式也不回收), 故表达式的
    //     “本树新增字节”对整棵子树求和 (过近似, 安全);
    //   * 控制流语句的条件用 incoming 求值; 每个体语句从其自己的
    //     incoming 起算 (体在 gen_stmt 外层 save/restore 内, 且 switch
    //     等的体可能复用外层为条件分配的槽);
    //   * switch 表达式分配的 16+8 槽存活至全部 case 体结束 — 取
    //     (24 + 体内最大水位)。
    pub fn max_sret_bytes(items: &[BlockItem]) -> i64 {
        items
            .iter()
            .map(Self::sret_bytes_block_item)
            .max()
            .unwrap_or(0)
    }

    fn sret_bytes_block_item(item: &BlockItem) -> i64 {
        match item {
            BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().map_or(0, Self::sret_bytes_expr),
            BlockItem::Stmt(s) => Self::sret_bytes_stmt(s),
            _ => 0,
        }
    }

    fn sret_bytes_stmt(s: &Stmt) -> i64 {
        match s {
            Stmt::Expr { e, .. } => Self::sret_bytes_expr(e),
            Stmt::Return { val: Some(e), .. } => Self::sret_bytes_expr(e),
            Stmt::If {
                cond, then_, else_, ..
            } => Self::sret_bytes_expr(cond)
                .max(Self::sret_bytes_stmt(then_))
                .max(else_.as_ref().map_or(0, |e| Self::sret_bytes_stmt(e))),
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::sret_bytes_expr(cond).max(Self::sret_bytes_stmt(body))
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => init
                .iter()
                .map(|s| Self::sret_bytes_stmt(s))
                .chain(cond.iter().map(Self::sret_bytes_expr))
                .chain(step.iter().map(Self::sret_bytes_expr))
                .max()
                .unwrap_or(0)
                .max(Self::sret_bytes_stmt(body)),
            Stmt::Block { b, .. } => Self::max_sret_bytes(&b.items),
            Stmt::Switch { expr, cases, .. } => {
                // expr 槽 (16+8) 存活至所有 case 体结束
                let base = Self::sret_bytes_expr(expr) + 24;
                cases
                    .iter()
                    .flat_map(|c| c.body.items.iter())
                    .map(Self::sret_bytes_block_item)
                    .max()
                    .unwrap_or(0)
                    .max(base)
            },
            Stmt::GotoIndirect { expr, .. } => Self::sret_bytes_expr(expr),
            _ => 0,
        }
    }

    /// 表达式树在一次求值内“新增的 sret/物化字节”峰值。按当前代码生成,
    /// 表达式求值期间不释放任何槽, 故对各分配站点与其子树求和。
    fn sret_bytes_expr(e: &Expr) -> i64 {
        let own = match e {
            Expr::Call {
                ty, callee, args, ..
            } => {
                let sret = if Self::needs_memory_return(ty) {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                // _Complex long double 返回 (经 x87, needs_memory_return
                // == false) 仍在调用侧复制到固定 result_off, 不经过
                // alloc_sret_slot, 故不占本区。
                sret + std::iter::once(callee.as_ref())
                    .map(Self::sret_bytes_expr)
                    .chain(args.iter().map(Self::sret_bytes_expr))
                    .max()
                    .unwrap_or(0)
            },
            Expr::VaArg { ty, ap, .. } => {
                // 只有真正走 `alloc_sret_slot` 的缓冲区交付才占本区；复数
                // `va_arg` 直接落帧底固定的复数物化带 (与 `gen_expr_va_arg`
                // 同源，见 `va_arg_result_in_cx_band`)。
                let own =
                    if Self::va_arg_result_in_memory(ty) && !Self::va_arg_result_in_cx_band(ty) {
                        (ty.size() as i64 + 15) & !15
                    } else {
                        0
                    };
                own + Self::sret_bytes_expr(ap)
            },
            Expr::Binop { l, r, .. } => {
                // _Complex long double 二元: 两个 32B 物化槽同时存活
                // (spill l, spill r), 结果走固定 result_off 不占本区。
                Self::complex_ld_sret_binop(e)
                    + Self::sret_bytes_expr(l).max(Self::sret_bytes_expr(r))
            },
            Expr::Assign { l, r, .. } => Self::sret_bytes_expr(l) + Self::sret_bytes_expr(r),
            Expr::StmtExpr { block, ty, .. } => {
                // 聚合 StmtExpr 分配一个类型槽; 内部块另算
                let own = if ty.size() > 0 {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                own + Self::max_sret_bytes(&block.items)
            },
            Expr::Comma { l, r, .. } => Self::sret_bytes_expr(l).max(Self::sret_bytes_expr(r)),
            Expr::Unary { e: inner, .. }
            | Expr::Cast { e: inner, .. }
            | Expr::AddrOf { e: inner, .. }
            | Expr::Deref { e: inner, .. }
            | Expr::PostInc { e: inner, .. }
            | Expr::PostDec { e: inner, .. }
            | Expr::PreInc { e: inner, .. }
            | Expr::PreDec { e: inner, .. } => Self::sret_bytes_expr(inner),
            Expr::Index { arr, idx, .. } => {
                Self::sret_bytes_expr(arr).max(Self::sret_bytes_expr(idx))
            },
            Expr::Member { expr, .. } => Self::sret_bytes_expr(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => Self::sret_bytes_expr(cond)
                .max(Self::sret_bytes_expr(then_))
                .max(Self::sret_bytes_expr(else_)),
            _ => 0,
        };
        own
    }

    /// 若表达式是 `_Complex long double` 二元 (含一元负/conj 等物化点),
    /// 返回其直接物化槽字节数; 否则 0。
    fn complex_ld_sret_binop(e: &Expr) -> i64 {
        let is_cld = |ty: &Type| {
            matches!(
                ty,
                Type::Complex(q) if matches!(q.base, Type::LongDouble)
            )
        };
        match e {
            Expr::Binop { ty, .. } if is_cld(ty) => 64, // 两个 32B 物化槽
            Expr::Unary { ty, .. } if is_cld(ty) => 32,
            _ => 0,
        }
    }

    /// 单个变量声明占用的 8 字节栈槽数 (分配口径的单一真源)。
    /// VLA 追加 16 字节元信息槽 (基址 + 运行时行大小 + 声明时总大小)。
    /// pre_register_vars 与 register_var 必须共用本函数, 否则两处对
    /// “变量占多大槽”的理解会漂移。
    pub fn var_alloc_slots(ty: &Type) -> i64 {
        let alloc_sz = if Self::type_has_vla(ty) {
            ty.size().saturating_add(16)
        } else {
            ty.size()
        };
        ((alloc_sz.saturating_add(7) / 8).max(1)) as i64
    }

    /// 规划期看不见、只在代码生成时才 `register_var` 的帧槽总量。
    ///
    /// 复合字面量临时对象 `__compound_lit_<pos>` (见 `gen_expr_compound_lit`) 与 VM
    /// struct 的尺寸元信息槽 `__vmsize_<tag>` (见 `gen_vm_type_bind`) 都不在
    /// `pre_register_vars` 能预知的名字集合里 (键含发射位置 / 类型标签), 但它们与
    /// 局部变量共用**同一条**向下生长的变量区水位。帧规划必须按同一口径
    /// (`var_alloc_slots`) 为它们预扣, 否则变量区会越过规划期定下的
    /// `sret_region_base` 而压住 sret/物化槽区 —— `a[i] = (struct S){...}` 里
    /// `a[i]` 的目标地址槽被字面量分量覆盖, `rep movsq` 拿 112 当目的地址段错误。
    pub fn late_var_slots(items: &[BlockItem]) -> i64 {
        items.iter().map(Self::late_var_slots_block_item).sum()
    }

    fn late_var_slots_decl(d: &Decl) -> i64 {
        match d {
            Decl::Var(v) => {
                let mut out = 0i64;
                if Self::type_has_vla(&v.ty) && Self::is_vm_record_ty(&v.ty) {
                    out += 1;
                }
                if let Some(e) = &v.init {
                    out += Self::late_var_slots_expr(e);
                }
                out
            },
            _ => 0,
        }
    }

    /// 与 `max_sret_bytes` 同骨架的语句遍历 (那条轴算 sret 区, 本条算变量区增量)。
    fn late_var_slots_stmt(s: &Stmt) -> i64 {
        match s {
            Stmt::Expr { e, .. } => Self::late_var_slots_expr(e),
            Stmt::Return { val, .. } => val.as_ref().map_or(0, Self::late_var_slots_expr),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::late_var_slots_expr(cond)
                    + Self::late_var_slots_stmt(then_)
                    + else_.as_ref().map_or(0, |s| Self::late_var_slots_stmt(s))
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::late_var_slots_expr(cond) + Self::late_var_slots_stmt(body)
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                init.as_ref().map_or(0, |s| Self::late_var_slots_stmt(s))
                    + cond.as_ref().map_or(0, Self::late_var_slots_expr)
                    + step.as_ref().map_or(0, Self::late_var_slots_expr)
                    + Self::late_var_slots_stmt(body)
            },
            Stmt::Block { b, .. } => Self::late_var_slots(&b.items),
            Stmt::GotoIndirect { expr, .. } => Self::late_var_slots_expr(expr),
            Stmt::Switch { expr, cases, .. } => {
                Self::late_var_slots_expr(expr)
                    + cases
                        .iter()
                        .map(|c| {
                            c.body
                                .items
                                .iter()
                                .map(Self::late_var_slots_block_item)
                                .sum::<i64>()
                        })
                        .sum::<i64>()
            },
            _ => 0,
        }
    }

    fn late_var_slots_block_item(item: &BlockItem) -> i64 {
        match item {
            BlockItem::Decl(d) => Self::late_var_slots_decl(d),
            BlockItem::Stmt(s) => Self::late_var_slots_stmt(s),
        }
    }

    /// 表达式树里每个复合字面量站点按其类型扣一份变量区槽位 (站点名含发射位置,
    /// 一次站点 = 一次 `register_var`); 初始化器与其余子表达式继续递归。
    fn late_var_slots_expr(e: &Expr) -> i64 {
        let own = match e {
            Expr::CompoundLit { ty, inits, .. } => {
                let vm = if Self::type_has_vla(ty) && Self::is_vm_record_ty(ty) {
                    1
                } else {
                    0
                };
                vm + Self::var_alloc_slots(ty)
                    + inits
                        .iter()
                        .map(|it| match it {
                            InitItem::Expr(ex) => Self::late_var_slots_expr(ex),
                            InitItem::Designated { value, .. } => Self::late_var_slots_expr(value),
                        })
                        .sum::<i64>()
            },
            Expr::InitList { inits, .. } => inits
                .iter()
                .map(|it| match it {
                    InitItem::Expr(ex) => Self::late_var_slots_expr(ex),
                    InitItem::Designated { value, .. } => Self::late_var_slots_expr(value),
                })
                .sum(),
            Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
                Self::late_var_slots_expr(l) + Self::late_var_slots_expr(r)
            },
            Expr::Unary { e: inner, .. }
            | Expr::Cast { e: inner, .. }
            | Expr::AddrOf { e: inner, .. }
            | Expr::Deref { e: inner, .. }
            | Expr::Real { e: inner, .. }
            | Expr::Imag { e: inner, .. }
            | Expr::SizeofExpr { e: inner, .. }
            | Expr::AlignofExpr { e: inner, .. }
            | Expr::PostInc { e: inner, .. }
            | Expr::PostDec { e: inner, .. }
            | Expr::PreInc { e: inner, .. }
            | Expr::PreDec { e: inner, .. } => Self::late_var_slots_expr(inner),
            Expr::Call { callee, args, .. } => {
                Self::late_var_slots_expr(callee)
                    + args.iter().map(Self::late_var_slots_expr).sum::<i64>()
            },
            Expr::Index { arr, idx, .. } => {
                Self::late_var_slots_expr(arr) + Self::late_var_slots_expr(idx)
            },
            Expr::Member { expr, .. } => Self::late_var_slots_expr(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                Self::late_var_slots_expr(cond)
                    + Self::late_var_slots_expr(then_)
                    + Self::late_var_slots_expr(else_)
            },
            Expr::StmtExpr { block, .. } => Self::late_var_slots(&block.items),
            Expr::VaArg { ap, .. } => Self::late_var_slots_expr(ap),
            Expr::Offsetof { indices, .. } => indices
                .iter()
                .map(|i| Self::late_var_slots_expr(i))
                .sum::<i64>(),
            Expr::AsmExpr {
                outputs, inputs, ..
            } => {
                outputs.iter().map(Self::late_var_slots_expr).sum::<i64>()
                    + inputs.iter().map(Self::late_var_slots_expr).sum::<i64>()
            },
            _ => 0,
        };
        own
    }

    /// struct / union 记录类型 (VM 尺寸元信息槽只服务于它们)。
    fn is_vm_record_ty(ty: &Type) -> bool {
        matches!(ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
    }

    /// 帧规划前把「规划期看不见的运行期临时对象」预扣进变量区水位。
    /// 两个帧规划分支 (带参数的普通函数 / `main`) 共用本函数, 避免其中一处
    /// 漏扣后又是一次“变量区压进 sret 区”的定向失效。
    pub fn reserve_late_var_slots(&mut self, body: Option<&Block>) {
        let slots = body.map_or(0, |b| Self::late_var_slots(&b.items));
        self.var_map_total_slots = self.var_map_total_slots.saturating_add(slots);
    }

    /// 收集函数体内每个变量名在**所有作用域**中的最大分配需求。
    /// var_map 按名字去重: 不同作用域的同名变量共享同一个栈槽 (这是
    /// 既有设计, 见 register_var 的遮蔽注释), 但槽容量必须取同名声明
    /// 的最大值。旧实现按**首次声明**定容, 后续同名更大的结构体声明
    /// (如先 `struct with_vec s;` 16B 后 `struct vec_int s;` 32B) 会
    /// 溢出到相邻局部 —— 根因: flag_clobber_frame.c 中 `s.i = 7`
    /// 静默覆盖已执行的 `ok = 0`, 退出码型夹具假绿。
    /// 遍历结构与 pre_register_stmt 保持一致, 跳过规则 (extern /
    /// static local) 也与 pre_register_vars 一致。
    fn collect_var_alloc_maxima(
        items: &[BlockItem],
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut HashMap<String, (i64, Type)>,
    ) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.is_extern && v.init.is_none() {
                        continue;
                    }
                    if v.is_static && static_local_map.is_static_local(func_name, &v.name.name) {
                        continue;
                    }
                    let slots = Self::var_alloc_slots(&v.ty);
                    match out.get(&v.name.name) {
                        Some(&(cur, _)) if cur >= slots => {},
                        _ => {
                            out.insert(v.name.name.clone(), (slots, v.ty.clone()));
                        },
                    }
                },
                BlockItem::Stmt(s) => {
                    Self::collect_var_alloc_maxima_stmt(s, static_local_map, func_name, out)
                },
                _ => {},
            }
        }
    }

    fn collect_var_alloc_maxima_stmt(
        s: &Stmt,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut HashMap<String, (i64, Type)>,
    ) {
        match s {
            Stmt::Block { b, .. } => {
                Self::collect_var_alloc_maxima(&b.items, static_local_map, func_name, out);
            },
            Stmt::If { then_, else_, .. } => {
                Self::collect_var_alloc_maxima_stmt(then_, static_local_map, func_name, out);
                if let Some(e) = else_ {
                    Self::collect_var_alloc_maxima_stmt(e, static_local_map, func_name, out);
                }
            },
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_var_alloc_maxima_stmt(body, static_local_map, func_name, out);
            },
            Stmt::Switch { cases, .. } => {
                for case in cases {
                    Self::collect_var_alloc_maxima(
                        &case.body.items,
                        static_local_map,
                        func_name,
                        out,
                    );
                }
            },
            _ => {},
        }
    }

    pub fn pre_register_vars(
        var_map: &mut HashMap<String, (i64, Type)>,
        items: &[BlockItem],
        temp_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
    ) {
        // 先收集每个名字的全函数最大分配需求, 再按首次出现顺序分配偏移。
        // maxima 必须从整棵函数体树算得并随递归全程携带: 若在嵌套
        // 调用里重算, 子树看不到其它作用域的同名更大声明, 会倒退回
        // 按首次声明定容的旧缺陷。
        let mut maxima: HashMap<String, (i64, Type)> = HashMap::new();
        Self::collect_var_alloc_maxima(items, static_local_map, func_name, &mut maxima);
        Self::pre_register_vars_max(
            var_map,
            items,
            temp_slots,
            kasan_rz_slots,
            static_local_map,
            func_name,
            total_slots,
            &maxima,
        );
    }

    fn pre_register_vars_max(
        var_map: &mut HashMap<String, (i64, Type)>,
        items: &[BlockItem],
        temp_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
        maxima: &HashMap<String, (i64, Type)>,
    ) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    // extern 声明引用全局符号, 不分配栈槽
                    if v.is_extern && v.init.is_none() {
                        continue;
                    }
                    if v.is_static {
                        // 由分析 pass 预注册的 static local 跳过栈分配
                        if static_local_map.is_static_local(func_name, &v.name.name) {
                            continue;
                        }
                    }
                    let (slots, alloc_ty) = match maxima.get(&v.name.name) {
                        Some((s, t)) => (*s, t.clone()),
                        None => (Self::var_alloc_slots(&v.ty), v.ty.clone()),
                    };
                    // 同名变量在不同作用域出现多次时, or_insert 不插入但
                    // total_slots 仍递增会导致偏移计算错误。
                    if !var_map.contains_key(&v.name.name) {
                        let used = total_slots.saturating_mul(8);
                        let hdr = 48i64 + temp_slots as i64 * 8;
                        let off = -(used + hdr) - (slots.saturating_sub(1) * 8);
                        var_map.insert(v.name.name.clone(), (off, alloc_ty));
                        *total_slots = total_slots.saturating_add(slots);
                        let rz_slots = kasan_rz_slots as i64;
                        *total_slots = total_slots.saturating_add(rz_slots);
                        for j in 0..kasan_rz_slots {
                            let rz_name = format!("__kasan_rz_{}_{}", v.name.name, j);
                            let rz_off = off - slots * 8 - j as i64 * 8;
                            var_map.entry(rz_name).or_insert((rz_off, Type::Void));
                        }
                    }
                },
                BlockItem::Stmt(s) => {
                    Self::pre_register_stmt_max(
                        var_map,
                        s,
                        temp_slots,
                        kasan_rz_slots,
                        static_local_map,
                        func_name,
                        total_slots,
                        maxima,
                    );
                },
                _ => {},
            }
        }
    }

    fn pre_register_stmt_max(
        var_map: &mut HashMap<String, (i64, Type)>,
        s: &Stmt,
        temp_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
        maxima: &HashMap<String, (i64, Type)>,
    ) {
        match s {
            Stmt::Block { b, .. } => {
                Self::pre_register_vars_max(
                    var_map,
                    &b.items,
                    temp_slots,
                    kasan_rz_slots,
                    static_local_map,
                    func_name,
                    total_slots,
                    maxima,
                );
            },
            Stmt::If { then_, else_, .. } => {
                Self::pre_register_stmt_max(
                    var_map,
                    then_,
                    temp_slots,
                    kasan_rz_slots,
                    static_local_map,
                    func_name,
                    total_slots,
                    maxima,
                );
                if let Some(e) = else_ {
                    Self::pre_register_stmt_max(
                        var_map,
                        e,
                        temp_slots,
                        kasan_rz_slots,
                        static_local_map,
                        func_name,
                        total_slots,
                        maxima,
                    );
                }
            },
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::pre_register_stmt_max(
                    var_map,
                    body,
                    temp_slots,
                    kasan_rz_slots,
                    static_local_map,
                    func_name,
                    total_slots,
                    maxima,
                );
            },
            Stmt::Switch { cases, .. } => {
                for case in cases {
                    Self::pre_register_vars_max(
                        var_map,
                        &case.body.items,
                        temp_slots,
                        kasan_rz_slots,
                        static_local_map,
                        func_name,
                        total_slots,
                        maxima,
                    );
                }
            },
            _ => {},
        }
    }

    pub fn collect_direct_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Func(f)) if f.body.is_some() => out.push(f.clone()),
                BlockItem::Stmt(s) => Self::collect_direct_nested_funcs_stmt(s, out),
                _ => {},
            }
        }
    }

    fn collect_direct_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_direct_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_direct_nested_funcs_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_direct_nested_funcs_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                Self::collect_direct_nested_funcs_stmt(body, out);
            },
            _ => {},
        }
    }

    pub fn emit_zero_init_frame(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        // 保存 rcx, rdi, rax (它们可能在后续代码中使用)
        self.emit(&[0x50]); // push rax
        self.emit(&[0x51]); // push rcx
        self.emit(&[0x57]); // push rdi

        // xor eax, eax (rax = 0)
        self.emit(&[0x31, 0xC0]);
        // mov rcx, frame_size / 8
        self.emit(&[0x48, 0xB9]);
        self.emit(&((frame_size as u64) / 8).to_le_bytes());
        // rdi = r13 - frame_size (帧底部)
        self.emit_lea_r13(7, -frame_size);
        // rep stosq: F3 48 AB
        self.emit(&[0xF3, 0x48, 0xAB]);

        // 恢复 rdi, rcx, rax
        self.emit(&[0x5F]); // pop rdi
        self.emit(&[0x59]); // pop rcx
        self.emit(&[0x58]); // pop rax
    }

    pub fn emit_stack_clash_probes(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        let page: i64 = 4096;
        let pages = frame_size / page;
        let remaining = frame_size % page;
        if pages > 0 {
            self.mov_imm(11, pages as i128);
            let loop_start = self.pos();
            self.emit(&[0x48, 0x81, 0xEC, 0x00, 0x10, 0x00, 0x00]); // sub rsp, 4096
            self.emit(&[0xC6, 0x04, 0x24, 0x00]); // mov byte ptr [rsp], 0
            self.emit(&[0x49, 0xFF, 0xCB]); // dec r11
            let loop_end = self.pos();
            let disp = loop_start as i64 - loop_end as i64 - 2;
            self.emit(&[disp as i8 as u8]); // jnz loop
        }
        self.emit_sub_rsp(remaining);
    }

    pub fn emit_xmm_r13_disp(&mut self, pfx: u8, op: u8, xmm: u8, off: i64) {
        let rex = 0x41 | if xmm >= 8 { 0x04 } else { 0 };
        if pfx != 0 {
            self.emit(&[pfx, rex, 0x0F, op]);
        } else {
            self.emit(&[rex, 0x0F, op]);
        }
        if Self::fits_disp32(off) {
            self.emit_r13_modrm_disp(xmm, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = 0x41 | if xmm >= 8 { 0x04 } else { 0 } | 0x01;
            if pfx != 0 {
                self.emit(&[pfx, rex, 0x0F, op]);
            } else {
                self.emit(&[rex, 0x0F, op]);
            }
            self.emit_reg_indirect_modrm(xmm, 11);
        }
    }

    pub fn store_xmm32_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0xF3, 0x11, xmm, off);
    }

    pub fn emit_func_epilogue(&mut self) {
        // Stack canary 检查: 在恢复 rsp 前验证金丝雀值
        if self.target.stack_protector && self.frame_size >= 8 {
            // mov r11, [r13 - 8]  → 加载存储的 canary 到临时寄存器
            self.emit(&[0x4d, 0x8b, 0x9d, 0xf8, 0xff, 0xff, 0xff]);
            // xor r11, seg:[0x28]  → 与原始 canary 比较
            // seg = fs (0x64) or gs (0x65) based on guard register
            let seg_prefix = match self.target.stack_protector_guard_reg {
                crate::cli::StackGuardReg::Gs => 0x65u8,
                _ => 0x64u8, // Fs is default
            };
            self.emit(&[seg_prefix, 0x4c, 0x33, 0x1c, 0x25, 0x28, 0x00, 0x00, 0x00]);
            // Always call __stack_chk_fail on canary mismatch.
            // Both ET_REL and ET_EXEC use the same path:
            // __stack_chk_fail is emitted as an undefined global
            // symbol (provided by libc / kernel runtime).
            self.emit(&[0x74, 0x05]); // je .+5 (canary OK → skip)
            self.call_label("__stack_chk_fail");
        }
        // 恢复被调用者保存寄存器: rbx, r12, r14, r15
        // 同序言: 无条件发射, 未使用的寄存器由 patch_callee_saves 改为 nop
        // mov rbx, [r13 - 16]  (REX.WB: r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x01));
        self.emit(&[0x49, 0x8b, 0x5d, 0xf0]);
        // mov r12, [r13 - 24]  (REX.WRB: r12 needs REX.R, r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x02));
        self.emit(&[0x4d, 0x8b, 0x65, 0xe8]);
        // mov r14, [r13 - 32]  (REX.WRB: r14 needs REX.R, r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x04));
        self.emit(&[0x4d, 0x8b, 0x75, 0xe0]);
        // mov r15, [r13 - 40]  (REX.WRB: r15 在 reg 字段需要 REX.R, r13 在 r/m 需要 REX.B)
        // 这里没有 SIB，REX.X 是多余的：旧代码发 0x4F = W|R|X|B，产物里被 objdump 标成
        // `rex.WRXB mov -0x28(%r13),%r15`（与序言侧同一个复制源）。
        self.callee_save_patch_sites.push((self.pos(), 0x08));
        self.emit(&[0x4d, 0x8b, 0x7d, 0xd8]);
        // mov rsp, r13: 恢复栈指针
        self.emit(&[0x49, 0x8b, 0xe5]);
        self.emit(&[0x41, 0x5d]); // pop r13
        if self.target.frame_pointer {
            self.emit(&[0x5D]); // pop rbp
        }
        // noreturn: 在 ret 前插入 trap 指令 (ud2)
        // 防止函数未正常返回时 (如 panic/abort/longjmp)
        // 执行流落入调用者后续代码
        if self.current_is_noreturn {
            self.emit(&[0x0F, 0x0B]); // ud2 - undefined instruction trap
        }
        self.emit_return_tail();
    }

    pub fn emit_return_tail(&mut self) {
        if self.target.function_return_thunk_extern {
            self.jmp_external("__x86_return_thunk");
        } else {
            self.emit(&[0xC3]);
        }
    }
}
