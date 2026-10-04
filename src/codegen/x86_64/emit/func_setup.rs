// src/codegen/x86_64/emit/func_setup.rs
// Per-function setup helpers: frame bookkeeping, local analysis walks and
// small register/stack helpers used by compile_func. Manually migrated from
// the pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::abi::layout::LayoutMap;
use crate::codegen::x86_64::Codegen;
use std::collections::HashMap;

/// 用户写了不是寄存器的寄存器名（与 `report_internal_bug` 不同：这是**用户代码错误**，
/// 不是后端不变量破坏）。
const DIAG_BAD_REG_NAME: &str = "E0316";

impl Codegen {
    pub fn reset_reg_tracker(&mut self) {
        // 占用真相只有一份：清的是 `RegBank` 里的两个位图与钉住集合。
        self.reg_bank.reset();
        // file-scope 的 `register asm` 绑定在 compile_func 开头就已经写进
        // reg_bindings, 而上面一行把占用清空。不重新 claim 就等于宣告
        // 「本函数不使用这些寄存器」, 序言里对应的保存会被 patch_callee_saves
        // 改成 nop —— 调用者存放在里面的值就此消失 (本地声明那条路径在绑定处
        // 当场 claim, 因为它的时机晚于本函数)。
        let bound: Vec<u8> = self
            .homes
            .values()
            .filter_map(|(l, _)| l.reg())
            .map(|r| r.n)
            .collect();
        for reg in bound {
            self.reg_bank.claim(reg);
        }
    }

    /// P12 Phase 5.2a：把「本函数里能把控制流带出保存/恢复区间的 asm goto」clobber 掉的
    /// 池号从临时值发号器里拿掉。
    ///
    /// 为什么不能只在发射点处理：守护是一对 `push`/`pop`，而 `asm goto` 的跳出路径上不
    /// 经过那个 `pop`。所以必须在函数入口就知道这件事（H1），不然第一个表达式已经把值
    /// 放进了那个号。
    ///
    /// 判据本身只有一份（`crate::ast_asm::escaping_asm_clobbers`）：分配器用同一个事实决
    /// 定「变量的家不得放这些号」（`ssa::regalloc::plan_for`）。两处各自写一遍遍历就会
    /// 漂移，而漂移的方向就是静默错值。
    pub fn ban_escaping_asm_regs(&mut self, body: Option<&crate::ast::Block>) {
        let Some(b) = body else { return };
        for name in crate::ast_asm::escaping_asm_clobbers(b) {
            // 名字→号走本后端已有的那一份解析（`register asm` 与 clobber 同一口径）；
            // `ban` 对池外的号（caller-saved）直接返 false，所以 `"rcx"` 这种最常见的
            // clobber 不会改变本函数能用的溅出号。
            if let Some(reg) = Self::parse_reg_name(&name) {
                self.reg_bank.ban(reg);
            }
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
    /// 函数体生成完毕后 `RegBank::used_mask` 已完整, 将未使用寄存器的
    /// 4 字节 mov 改写为 4×0x90 (nop), 消除叶子函数的冗余保存/恢复.
    /// 尾声可能在每个 return 处内联发射, 统一在此处回填, 不依赖生成顺序.
    /// sites_base: compile_func 开头记录的 sites 起始下标 (嵌套函数隔离).
    pub fn patch_callee_saves(&mut self, sites_base: usize) {
        let used = self.reg_bank.used_mask();
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

    /// 用户文本名 → 家（后端事实：名字表只有 `parse_reg_name` 这一份）。
    /// 名字不是本目标已知的寄存器 ⇒ **报错**，而不是静默不绑。
    ///
    /// 实测过的静默后果（P12 Phase 4.1d 要消除的缺陷）：
    /// `register long gv asm("nosuchreg");` 在改前退出码 0，而产物里 `gv` 出现 **0 次** ——
    /// 全局变量因收集阶段 `continue` 不进 .data/.bss，又因 `rel_symbols` 判
    /// `register_name.is_some()` 不发符号，于是这个对象**整个消失**；而 gcc 在这里是
    /// `error: invalid register name for ‘gv’`。「一个名字没认出来」不可能比「声明不存在」
    /// 更接近用户意图，所以只能报错。
    pub(crate) fn resolve_asm_reg(&mut self, v: &VarDecl) -> Option<crate::location::Location> {
        let text = v.register_name.as_deref()?;
        if let Some(loc) = Self::parse_reg_home(text) {
            // 名字认得，但这个号是本端自己的帧基座：拿它当家会让所有 `%r13+disp` 寻址
            // 一起失效。而今天它不报错：实测同一个 `register long v asm("r13")` 在 -O0
            // 真发出 `mov %rax,%r13`，在 -O2 则被静默降级成栈槽（`mov %rax,-0x48(%r13)`）
            // —— 同一份源两个档位两个答案，那比报错坏得多。
            if let Some(why) = self.reserved_reg_reason(&loc) {
                self.diag_reserved_reg(&v.name.name, text, why, &v.span);
                return None;
            }
            return Some(loc);
        }
        self.diagnostics.push(crate::diagnostics::Diagnostic {
            sev: crate::diagnostics::Severity::Error,
            code: DIAG_BAD_REG_NAME,
            msg: format!("invalid register name for '{}': '{}'", v.name.name, text),
            span: v.span.clone(),
            hint: Some("register asm(\"rN\") 里的 N 必须是本目标已知的寄存器名".to_string()),
            source_line: None,
        });
        None
    }

    /// 保留号（栈指针 / 帧基址 / 帧指针）不得当变量之家：诊断码也只定在这里一处
    /// （`asm/emit.rs` 的 clobber 侧引用同一个常量）。
    pub(crate) const DIAG_RESERVED_REG: &'static str = "E0318";

    /// 这个家落在的号是不是本端保留号（帧基址 / 帧指针）。
    ///
    /// **只拒这两个，不拒 `%rsp`**：内核与 libm 依赖的
    /// `register unsigned long current_stack_pointer asm("rsp")` 是一个合法且常见的
    /// 「只读别名」（本轮实测：把它拒了直接弄坏 `parallel_codegen_keeps_file_scope_register_asm_binding`）。
    /// 家为 `Reg(4)` 时读侧发的就是 `mov %rsp,%rax`，正是用户要的东西；写侧也是用户自己要求的。
    /// 而 `%r13`/`%rbp` 不一样：它们是本端自己用来定位帧的，被当家用会让所有
    /// `%r13+disp` 寻址一起失效（或在另一个档位被静默降级）。
    /// clobber 侧的 `%rsp` 仍被拒（它守无可守），见 `asm/emit.rs::asm_reject_reserved_clobber`。
    fn reserved_reg_reason(&self, loc: &crate::location::Location) -> Option<&'static str> {
        let n = loc.reg()?.n;
        match n {
            13 => Some("%r13 是本后端的帧基址：所有局部量都按 `%r13+disp` 寻址"),
            5 if self.target.frame_pointer => {
                Some("%rbp 在 -fno-omit-frame-pointer 下是帧指针（序言 push、尾声 pop）")
            },
            _ => None,
        }
    }

    fn diag_reserved_reg(&mut self, name: &str, text: &str, why: &str, span: &crate::diagnostics::Span) {
        use crate::diagnostics::{Diagnostic, Severity};
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: Self::DIAG_RESERVED_REG,
            msg: format!("register '{}': '{}' 是保留寄存器 —— {}", name, text, why),
            span: span.clone(),
            hint: Some(
                "本后端可当变量之家的 callee-saved 号只有 rbx/r12/r14/r15（%r13 是帧基址、\n\
                 %rbp 在 -fno-omit-frame-pointer 下是帧指针，都不行；%rsp 当只读别名是可以的）".to_string(),
            ),
            source_line: None,
        });
    }

    /// 变量声明处的**家**写入的唯一入口（P12 Phase 4.1c）。
    ///
    /// 为什么要有它：同一件事（「这个声明要住哪个寄存器」）之前在 `compile_func` 与
    /// `gen_block_items` 各有一份拷贝（两处字面同形、改一处漏一处的形状）；而 §7.15/§7.19
    /// 反复实测到的结论是「消费者没读同一张表」就是静默错值的来源。家在这里只有一个
    /// 算式，插入后必然同步 `RegBank` 与钉住集合。
    ///
    /// 两个来源的优先级与今天一致：**用户写的文本名 > IR 位置表给出的分配结果**
    /// （`apply_bindings` 本来就不会覆盖已有的 `register_name`，而自动分配把
    /// `already-register-asm` 当拒因，所以两者不会同时出现在同一个变量上）。
    /// 返回是否真的绑上了。
    pub fn bind_decl_home(&mut self, v: &VarDecl) -> bool {
        // 地址被取走的对象不得绑到物理寄存器：绑定后那个栈槽再没人写过，`&v` 的读者
        // 拿到旧值/垃圾（GCC 遇到逸出同样是忽略绑定，而规划期总是先分好了槽，
        // 退回内存路径无风险）。
        if !self.register_binding_allowed(&v.name.name) {
            return false;
        }
        let home = match &v.register_name {
            // 用户文本名：非法就报错（见 `resolve_asm_reg`），不静默降级成内存变量。
            Some(_) => self.resolve_asm_reg(v),
            // 自动分配的结果：已是 `Location`，不再走名字这一圈。
            None => v.home,
        };
        match home {
            Some(loc) => {
                self.insert_home(&v.name.name, loc, v.ty.clone());
                true
            }
            None => false,
        }
    }

    /// 登记一个家，并同步占用事实。**只给声明处用**：file-scope 表 (`global_homes`) 的
    /// 重填走直接 `insert`，因为那条路径上旧代码不在这里 claim（它在 `reset_reg_tracker`
    /// 里做），而 `naked` 函数根本不会走到 reset —— 改在这里 claim 就是新增行为。
    pub(super) fn insert_home(&mut self, name: &str, loc: crate::location::Location, ty: Type) {
        self.homes.insert(name.to_string(), (loc, ty));
        if let Some(r) = loc.reg() {
            // 绑定即占用该寄存器：必须保存 + 不得再当临时值发出（见 `RegBank::claim`）
            self.reg_bank.claim(r.n);
        }
    }

    /// 用户文本名 → 家（后端事实：名字表只有 `parse_reg_name` 这一份）。
    pub fn parse_reg_home(name: &str) -> Option<crate::location::Location> {
        Self::parse_reg_name(name).map(|n| {
            crate::location::Location::Reg(crate::location::RegId {
                bank: crate::location::RegBank::Gp,
                n,
            })
        })
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
        spill_slots: &mut i64,
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
            spill_slots,
        );
    }

    /// 本函数需要多少个 spill 槽：从**已经交付到声明上的家**里数（P12 Phase 5.1.b）。
    ///
    /// 为什么不在这里重新算一遍分配：分配器的决定已经写进 `VarDecl::home`，帧只需要回答
    /// 「这些家里出现过的最大槽号是多少」。拿 `max(槽号)+1` 当带尺寸，尺寸与坐标就天生
    /// 来自同一份事实；若另加一个 `FuncDecl::spill_slots` 字段由分配器写，就会出现
    /// 「带尺寸与槽号不一致」这种只能靠自觉维护的双轨（§3 的 H4）。
    ///
    /// 只算**真正占帧内槽**的声明：extern 引用全局符号、已由分析 pass 预注册的 static local
    /// 不占帧内空间（上面两处 `continue` 早于本行），所以它们就算带着 Spill 家也不得拉大带。
    /// 序号负值按非法忽略（宁可少给一个槽也不能算出一个越界的偏移）。
    fn count_spill_slot(v: &crate::ast::VarDecl, spill_slots: &mut i64) {
        if let Some(crate::location::Location::Spill(slot)) = v.home {
            if slot >= 0 && slot.saturating_add(1) > *spill_slots {
                *spill_slots = slot.saturating_add(1);
            }
        }
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
        spill_slots: &mut i64,
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
                    // 同一个逐声明循环顺带定 spill band 尺寸（不另开一份遍历）。
                    Self::count_spill_slot(v, spill_slots);
                    let (slots, alloc_ty) = match maxima.get(&v.name.name) {
                        Some((s, t)) => (*s, t.clone()),
                        None => (Self::var_alloc_slots(&v.ty), v.ty.clone()),
                    };
                    // 同名变量在不同作用域出现多次时, or_insert 不插入但
                    // total_slots 仍递增会导致偏移计算错误。
                    if !var_map.contains_key(&v.name.name) {
                        let used = total_slots.saturating_mul(8);
                        let hdr = LayoutMap::hdr_of(temp_slots as i64);
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
                        spill_slots,
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
        spill_slots: &mut i64,
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
                    spill_slots,
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
                    spill_slots,
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
                        spill_slots,
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
                    spill_slots,
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
                        spill_slots,
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
        if self.target.stack_protector && self.frame_layout.canary_fits() {
            // mov r11, [r13 - 8]  → 加载存储的 canary 到临时寄存器（槽位 = `abi::layout::CANARY_OFF`）
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BlockItem, Decl, Stmt};
    use crate::location::Location;
    use crate::static_locals::StaticLocalMap;

    /// 把指定名字的声明的 `home` 改成 `Spill(slot)`。今天分配器还不产出 Spill 家，所以
    /// 本用例只能这样注入 —— 验的是「带的尺寸从已交付的家导出」这条机制本身，
    /// 而不是某个分配结果。
    fn body_with_spill_home(src: &str, fname: &str, name: &str, slot: i64) -> Vec<BlockItem> {
        let mut items = body_of(src, fname);
        let mut hit = false;
        set_home(&mut items, name, slot, &mut hit);
        assert!(hit, "源里没有名为 '{name}' 的声明（用例不得因为找不到名字而空跑）");
        items
    }

    fn body_of(src: &str, fname: &str) -> Vec<BlockItem> {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        m.source()
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == fname => Some(f.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("源里没有函数 {fname}"))
            .body
            .expect("被测函数必须有体")
            .items
    }

    /// 遍历形状与 `pre_register_vars` 那一份一致（只服务于测试，不进产物路径）。
    fn set_home(items: &mut [BlockItem], name: &str, slot: i64, hit: &mut bool) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.name.name == name {
                        v.home = Some(Location::Spill(slot));
                        *hit = true;
                    }
                },
                // 其余声明（函数/类型/typedef/global asm）里没有要改的局部家。
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => set_home_stmt(s, name, slot, hit),
            }
        }
    }

    fn set_home_stmt(s: &mut Stmt, name: &str, slot: i64, hit: &mut bool) {
        match s {
            Stmt::Block { b, .. } => set_home(&mut b.items, name, slot, hit),
            Stmt::If { then_, else_, .. } => {
                set_home_stmt(then_, name, slot, hit);
                if let Some(e) = else_ {
                    set_home_stmt(e, name, slot, hit);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                set_home_stmt(body, name, slot, hit)
            },
            Stmt::Switch { cases, .. } => {
                for c in cases {
                    set_home(&mut c.body.items, name, slot, hit);
                }
            },
            _ => {},
        }
    }

    fn plan_spill_slots(items: &[BlockItem], fname: &str) -> (i64, usize, i64) {
        let mut var_map: HashMap<String, (i64, crate::ast::Type)> = HashMap::new();
        let mut total = 0i64;
        let mut spill = 0i64;
        Codegen::pre_register_vars(
            &mut var_map,
            items,
            2,
            0,
            &StaticLocalMap::default(),
            fname,
            &mut total,
            &mut spill,
        );
        // 第三个读数就是「遍历真的跑了」的证据：没有它，spill == 0 可能只是根本没进入循环。
        (spill, var_map.len(), total)
    }

    const SRC: &str = "long f(long n){ long a = n; { long deep = n + 1; a += deep; } \
                        extern long g_env; long b = a + n; return b + g_env; }\n";

    /// Phase 5.1.b 的三条：**最大槽号 + 1 就是带的尺寸**；嵌套块里的声明不会漏算；
    /// 而 extern 声明不占帧内空间，就算带着 Spill 家也不得拉大带（拉大了是白吃帧，
    /// 算小了就是越界写）。最后一句钉「没有 Spill 家⇒带为 0」：那才是今天全语料的形状，
    /// 也是「本步产物逐字节不变」的根据。
    #[test]
    fn spill_band_is_sized_from_the_delivered_homes() {
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "a", 0), "f").0, 1);
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "a", 2), "f").0, 3);
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "deep", 5), "f").0, 6);
        // extern 声明不占帧内槽：就算它被塞了 Spill 家也不得拉大带（拉大了就是白吃帧），
        // 而同一个调用里普通局部仍然真的被预注册（否则这条 0 只是空跑）。
        let (for_extern, registered, watermark) =
            plan_spill_slots(&body_with_spill_home(SRC, "f", "g_env", 7), "f");
        assert_eq!(for_extern, 0, "extern 声明不得拉大 spill band");
        assert!(
            registered >= 3 && watermark > 0,
            "同一个调用必须真的预注册了局部（变量区没算起来就是遍历没跑到，上面那个 0 就是假绿）"
        );
        let (none, _, _) = plan_spill_slots(&body_of(SRC, "f"), "f");
        assert_eq!(none, 0, "没有任何 Spill 家时带必须是 0");
    }
}
