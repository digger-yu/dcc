// src/codegen/x86_64/emit/functions.rs
// Function-level driver: walks the program's function definitions and
// delegates each one to compile_func. Manually migrated from the
// pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::abi::layout::LayoutMap;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{AbiClass, Codegen};
use std::collections::HashMap;

impl Codegen {
    pub fn emit_functions(&mut self, prog: &Program) {
        // Collect names of __attribute__((copy(fn))) functions: they must
        // not emit code because their namesake symbol is replaced by an
        // alias.
        let copy_fn_names: std::collections::HashSet<String> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    f.alias_name.as_ref().map(|_| f.name.name.clone())
                } else {
                    None
                }
            })
            .collect();
        let top_funcs: Vec<FuncDecl> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    // copy(fn) -> alias_name=Some(fn); do not emit a body.
                    if f.alias_name.is_some() {
                        return None;
                    }
                    // Same name as an earlier copy-attribute decl: treat
                    // this decl as an alias and skip its body.
                    if copy_fn_names.contains(&f.name.name) {
                        return None;
                    }
                    // __attribute__((error("..."))): compile-time assertion
                    // only (e.g. kernel __compiletime_assert_*); never emit
                    // a body even if the parser attached one.
                    if f.error_msg.is_some() {
                        return None;
                    }
                    if std::env::var_os("DCC_SKIP_AI_BODY").is_some()
                        && f.is_always_inline
                        && f.is_static
                    {
                        return None;
                    }
                    if f.body.is_some() {
                        Some(f.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        // 排障用：`DCC_EMIT_TRACE=1` 打出本 TU 真正进入发射的函数名。判定
        // 「某个函数体/内联副本漏发射或误发射」时，这是唯一能分清
        // 「根本没发射」与「发射了但诊断把名字记成 callee」的观测点。
        if std::env::var_os("DCC_EMIT_TRACE").is_some() {
            for f in &top_funcs {
                eprintln!(
                    "EMIT| {} static={} always_inline={} body_items={}",
                    f.name.name,
                    f.is_static,
                    f.is_always_inline,
                    f.body.as_ref().map(|b| b.items.len()).unwrap_or(0)
                );
            }
        }
        // 临时探针（DCC_DUMP_LEFTOVER）：列出「内联之后仍被直接调用」的强制内联函数，
        // 用来定位“某个调用点没能内联、于是要发射一份带 asm goto 的外带副本”的现场。
        if std::env::var_os("DCC_DUMP_LEFTOVER").is_some() {
            let names: std::collections::HashSet<String> = top_funcs
                .iter()
                .map(|f| f.name.name.clone())
                .collect();
            for f in &top_funcs {
                let (calls, _) = crate::ssa::passes::collect_func_refs(f, &names);
                let mut left: Vec<&String> = calls
                    .iter()
                    .filter(|c| {
                        top_funcs
                            .iter()
                            .any(|g| g.name.name == **c && g.is_always_inline)
                    })
                    .collect();
                left.sort();
                left.dedup();
                if !left.is_empty() {
                    eprintln!("LEFTOVER {} -> {:?}", f.name.name, left);
                }
            }
        }
        if self.parallel_codegen && self.relocatable && top_funcs.len() >= 2 {
            self.emit_functions_parallel(&top_funcs, prog);
            return;
        }
        // 诊断窗口（`DCC_FN_RSS=1`）：逐函数打印本进程 RSS 与耗时。
        // 为什么必须有：单 TU 的 RSS 会随函数数单调上涨（实测 `net/unix/af_unix.c`
        // 单跑爬到 6.7 GB 被 OOM 杀，而 `-fsyntax-only` 只有 247 MB、`-O0` 照涨），
        // 没有逐函数读数就分不清「某个函数一次爆」与「每个函数都留下一块不回收的
        // 内存」—— 两种根因的修法完全不同。只写 stderr，不进产物字节。
        let fn_rss = std::env::var_os("DCC_FN_RSS").is_some();
        for f in &top_funcs {
            let r0 = fn_rss.then(crate::opt::memdbg_rss_kb);
            let t0 = fn_rss.then(std::time::Instant::now);
            self.compile_func(f, prog);
            if let (Some(r0), Some(t0)) = (r0, t0) {
                let now = crate::opt::memdbg_rss_kb();
                eprintln!(
                    "dcc: fn-rss {} rss={}kB (+{}kB) dt={}ms",
                    f.name.name,
                    now,
                    now.saturating_sub(r0),
                    t0.elapsed().as_millis()
                );
            }
        }
    }
    /// 汇总全程序里「地址被取走」的对象根名，一个 Codegen 实例只算一次。
    ///
    /// 为什么是程序级而不是当前函数级：file-scope 的 `register asm` 变量可以在
    /// **任何**一个函数里被取址，只看本函数体会漏。判据本身复用
    /// `ast_gvn::addr_escaped_names`，后端不再自己爬一遍 AST —— 两套逸出判据里弱的
    /// 那一套会决定产物正确性。集合按名字索引，所以同名变量在别处被取址会一并
    /// 取消绑定（偏保守：只丢绑定，不丢正确性）。
    fn collect_addr_escaped(&mut self, prog: &Program) {
        if self.addr_escaped_done {
            return;
        }
        self.addr_escaped_done = true;
        // 只递一个入口：嵌套函数体由判据本身递归（`ast_gvn::walk_addr_of` 的
        // `Decl::Func` 臂）。后端再手写一份递归就是第二套逸出判据，
        // 而两套里弱的那一套会决定产物（§11.13 的 N8）。
        for d in &prog.decls {
            let crate::ast::Decl::Func(f) = d else {
                continue;
            };
            let Some(b) = &f.body else {
                continue;
            };
            self.addr_escaped
                .extend(crate::ast_gvn::addr_escaped_names(b));
        }
    }

    /// 该名字是否允许绑定到物理寄存器（地址逸出者不允许）。声明处的两条路径共用。
    pub(crate) fn register_binding_allowed(&self, name: &str) -> bool {
        !self.addr_escaped.contains(name)
    }

    /// 汇总全程序里「被内联汇编操作数引用」的对象根名，一个 Codegen 实例只算一次。
    ///
    /// 消费方是 `func_setup::resolve_asm_reg`：GNU C 的 `register T v asm("rN")` 只在
    /// asm 操作数里是可观察约定（GCC 文档的 supported use），未被引用、又在声明处有
    /// 初值的声明按普通变量处理 —— `asmgoto-5.c` 的 14 个 `register int vN asm("rXX") = aN`
    /// 就是这个形状（它们一个也没进 asm 模板，GCC 直接当普通局部变量）。
    ///
    /// 为什么程序级：file-scope 的绑定可以在**另一个**函数里被 asm 引用，而
    /// `compile_rel` 要在任何函数体发射之前给定它的家。判据单源在
    /// `ast_asm::program_asm_operand_names`（穷举遍历，含嵌套函数体）。
    pub(crate) fn collect_asm_operand_names(&mut self, prog: &Program) {
        if self.asm_operand_names_done {
            return;
        }
        self.asm_operand_names_done = true;
        self.asm_operand_names = crate::ast_asm::program_asm_operand_names(prog);
    }

    /// 递归收集块内（含嵌套语句）的全部局部变量声明。
    ///
    /// 只取声明、不取表达式：本函数只用来统计「这个名字在函数里被声明了几次」。
    /// `Switch` 的两块（`prelude` 与各 `case.body`）都要走 —— 漏了一处，
    /// 同一名字在两处声明就会被误判成「唯一」。
    fn collect_local_decls<'a>(
        items: &'a [crate::ast::BlockItem],
        out: &mut Vec<&'a crate::ast::VarDecl>,
    ) {
        use crate::ast::{BlockItem, Decl};
        for it in items {
            match it {
                BlockItem::Decl(Decl::Var(v)) => out.push(v),
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => Self::collect_stmt_decls(s, out),
            }
        }
    }

    fn collect_stmt_decls<'a>(s: &'a crate::ast::Stmt, out: &mut Vec<&'a crate::ast::VarDecl>) {
        use crate::ast::Stmt;
        match s {
            Stmt::If { then_, else_, .. } => {
                Self::collect_stmt_decls(then_, out);
                if let Some(e) = else_ {
                    Self::collect_stmt_decls(e, out);
                }
            },
            Stmt::While { body, .. }
            | Stmt::Do { body, .. }
            | Stmt::Label { stmt: body, .. } => Self::collect_stmt_decls(body, out),
            // `for` 的声明在 `init` 里（`for (int i = 0; ...)`）—— 漏掉它就会让
            // 一个真住在池号上的循环变量跑到计划外（实测 `-O2` 冷格外置夹具的
            // `i` 落 r12 ⇒ E1137）。
            Stmt::For { init, body, .. } => {
                if let Some(i) = init {
                    Self::collect_stmt_decls(i, out);
                }
                Self::collect_stmt_decls(body, out);
            },
            Stmt::Block { b, .. } => Self::collect_local_decls(&b.items, out),
            Stmt::Switch { cases, prelude, .. } => {
                Self::collect_local_decls(prelude, out);
                for c in cases {
                    Self::collect_local_decls(&c.body.items, out);
                }
            },
            Stmt::Invoke { call, .. } => Self::collect_stmt_decls(call, out),
            _ => {},
        }
    }

    /// `-fstack-protector-strong` 的逐函数准入判据（GCC `cfgexpand.c` 口径）。
    ///
    /// GCC 对 strong 的定义是「basic 的判据 + 数组局部 + 取局部地址」，不是
    /// 「所有函数都插」：只有标量局部的函数（内核 `native_write_cr4()` 就是一例）
    /// 不插桩。这不是加固口味问题 —— `x86_64_start_kernel()` 在本 CPU 的 GS 基址
    /// 建立之前就调 `native_write_cr4()`，无条件插 canary 会在 `%gs:0x28` 上取线性
    /// 地址 0x28，直接 #PF → #DF，PVH 启动卡在这里。
    ///
    /// 判据只取保守方向：漏插只会降低加固强度，不会改变语义；
    /// - 数组局部 / 含 VLA 的局部（`Type::Array` 或 `has_vla()`）；
    /// - 地址被取的局部/形参（`ast_gvn::addr_escaped_names` 的根名 ∩ 本函数绑定名）；
    /// - 显式 `alloca` 调用本轮未收（同上，只影响加固强度）。
    fn func_canary_needed(&self, f: &FuncDecl) -> bool {
        if !self.target.stack_protector_strong {
            // basic / all：保持原有「所有函数都插」的行为。
            return true;
        }
        let Some(body) = &f.body else { return false };
        if f.is_naked {
            return false;
        }
        let mut locals = Vec::new();
        Self::collect_local_decls(&body.items, &mut locals);
        if locals
            .iter()
            .any(|v| matches!(v.ty, Type::Array(..)) || v.ty.has_vla())
        {
            return true;
        }
        let escaped = crate::ast_gvn::addr_escaped_names(body);
        if escaped.is_empty() {
            return false;
        }
        let mut bound: std::collections::HashSet<&str> =
            locals.iter().map(|v| v.name.name.as_str()).collect();
        for p in &f.params {
            if let Some(pn) = &p.name {
                bound.insert(pn.name.as_str());
            }
        }
        escaped.iter().any(|n| bound.contains(n.as_str()))
    }

    /// 给**没走 SSA**的函数挑一批热局部变量，让它们住进 callee-saved 号。
    ///
    /// 为什么需要：`VarDecl::home` 只有 SSA 分配器会写（`ssa::regalloc`），而
    /// 含 `goto`/computed-goto/`switch` 的函数整函数退出 SSA（`is_structured`）。
    /// `luaV_execute` 正是这种：于是它的 `base`/`pc`/`cl`/`k` 每次都从栈上取，
    /// 静态栈引用是 gcc 的 10 倍。而 fib(32) 的耗时比 ≈ 静态指令数比（实测两边
    /// 都是 4.3x），说明每个 opcode 多发的指令就是全部代价 —— 不存在「指令多但
    /// 被乱序执行掩盖」这回事。
    ///
    /// 为什么是 callee-saved 而不是 caller-saved：`register asm` 那种**用户显式**
    /// 绑定的语义是「调用后可能被破坏」，每次 call 前后都要存/取；实测手绑
    /// `base`/`pc`/`cl`/`k` 后 mov-rr 少了 1815 条，栈引用反而**多了 1518 条**。
    /// 后端自选的家是优化，必须跨 call 正确 ⇒ 只能落在 callee-saved 上，那样
    /// 不需要任何存取，两端都减。
    ///
    /// 判据全部是保守的，任何一条不满足就退回栈槽（不绑是不会错的，绑错是静默错值）：
    /// - 名字在**整个函数**里只声明一次（同名遮蔽时 `homes` 只有一张表，两个对象
    ///   会共用一个号）；
    /// - 类型可绑（`ssa::regalloc::bindable_ty`）、宽度 1..=8、非静态存储、非 volatile、
    ///   非段地址空间、非 VLA；
    /// - 地址未被取走（`register_binding_allowed`：绑定后那个栈槽再没人写，
    ///   `&v` 的读者会拿到旧值）；
    /// - 该名字还没有家（用户 `register asm` 优先，file-scope 绑定也不覆盖）。
    fn plan_auto_homes(&mut self, f: &FuncDecl) {
        let Some(body) = &f.body else { return };
        // 本后端能当变量之家的 callee-saved 号：%r13 是帧基址、%rbp 在
        // -fno-omit-frame-pointer 下是帧指针、%rsp 是栈顶，三者都不行。
        // 顺序进产物，所以按既有分配器的习惯从小到大。
        //
        // 号数可调（`DCC_AUTO_HOME=0..4`，**默认 0 即关闭**）。
        //
        // 实测（`luaV_execute`，-O2）：打开 1/2/3/4 个号时指令数
        // 16810/16810/16809/16662，栈引用 3616/4030/4282/4708 —— mov-rr 减多少，
        // 栈引用就增多少，两边精确互补。原因很硬：这些号与 Binop 的临时值
        // **同一个池**，变量占一个，暂存就只能落帧槽。没有 CFG 活性就分不清
        // 「这个变量在这个区间真的值一个号」，在 emit 层分只能做成零和。
        // 开关留着是为了不靠记忆重复这个实验；它不是第二套行为。
        const POOL: [u8; 4] = [3, 12, 14, 15];
        let budget: usize = std::env::var("DCC_AUTO_HOME")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0)
            .min(POOL.len());
        if budget == 0 {
            return;
        }
        let pool = &POOL[..budget];
        let mut decls: Vec<&crate::ast::VarDecl> = Vec::new();
        Self::collect_local_decls(&body.items, &mut decls);
        if decls.is_empty() {
            return;
        }
        // 名字 → 声明次数。只绑「唯一一次」的名字。
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for v in &decls {
            *seen.entry(v.name.name.as_str()).or_insert(0) += 1;
        }
        let mut used: std::collections::HashSet<u8> = self
            .homes
            .values()
            .filter_map(|(l, _)| l.reg())
            .map(|r| r.n)
            .collect();
        let mut pool_idx = 0usize;
        for v in decls {
            if pool_idx >= pool.len() {
                break;
            }
            let name = &v.name.name;
            if seen.get(name.as_str()).copied() != Some(1) {
                continue;
            }
            if self.auto_homes.contains_key(name) || self.homes.contains_key(name) {
                continue;
            }
            if v.is_static || v.is_extern || v.is_thread_local {
                continue;
            }
            if v.register_name.is_some() || v.home.is_some() {
                continue;
            }
            if v.quals.is_volatile() || !matches!(v.addr_space, crate::ast::AddrSpace::None) {
                continue;
            }
            let sz = v.ty.size();
            if sz == 0 || sz > 8 {
                continue;
            }
            if !crate::ssa::regalloc::bindable_ty(&v.ty) {
                continue;
            }
            if !self.register_binding_allowed(name) {
                continue;
            }
            // 取一个还没被占的号；被占就跳过它（不挤掉用户/全局的绑定）。
            let mut reg = None;
            while pool_idx < pool.len() {
                let cand = pool[pool_idx];
                pool_idx += 1;
                if used.insert(cand) {
                    reg = Some(cand);
                    break;
                }
            }
            let Some(reg) = reg else { break };
            let Some(loc) = Self::reg_home_location(reg) else {
                continue;
            };
            self.auto_homes.insert(name.clone(), loc);
            self.homes.insert(name.clone(), (loc, v.ty.clone()));
        }
    }

    /// 本函数帧头要开几个 callee-saved 保存槽的**计划位图**（报告 §5.42）。
    ///
    /// 为什么必须是个静态计划而不是「发完了再数」：序言里的 `sub $帧长`、变量区顶、
    /// temp 槽坐标全部在函数体发射前就算完（单遍发射），所以帧头的 cs 槽数一旦
    /// 定下就回不了头。旧口径恒给 4 个槽（32B）+ 8B 洞，内核 trace 上 119 帧里平均
    /// 只真用 1.11 个号 —— 白付接近 3KB，直接吃掉 16KB 任务栈的余量。
    ///
    /// 计划来源（全部在发射前可定稿）：
    ///  1. 已定稿的家表：file-scope `register asm`、形参家（`DCC_HOME_PARAMS`）、
    ///     后端自选家（`plan_auto_homes`）；
    ///  2. 函数体里**所有**局部声明：分配器写的 `home` 与用户写的 `register_name`
    ///     （`collect_local_decls` 是递归的，块内/语句表达式里的都收）；
    ///  3. 内联汇编：固定寄存器约束点名的池号（`"b"`→rbx、显式名 `{r12}` 等，
    ///     判据单源于 `fixed_gp_of_constraint`）；另外只要含 asm 就留 r12 ——
    ///     `alloc_asm_mem_scratch` 池耗尽时回退它（旧行为），不保存就是毁调用者。
    ///
    /// 计划之外的号在发射期不会被发出（`RegBank::set_plan_mask` 的过滤），
    /// 而 `claim` 撞图外号会被记下并由收尾报 `E1137`（fail-closed）。
    pub(crate) fn compute_callee_plan(&self, f: &FuncDecl) -> u8 {
        use crate::codegen::x86_64::abi::regbank::RegBank;
        // 验收/回归用的 A/B 开关：强制旧口径（四个号全部预留）。帧几何在新公式下
        // 与旧 `48 + 8*temps` 逐字节相等（`hdr_of` 的 16 = 48 - 4*8），所以这一个
        // 环境变量就能把「按需化前/后」做成**同一编译器**上的对照，而不是拿两个
        // 不同日期的二进制比帧。未设置时零副作用。
        if std::env::var_os("DCC_CS_PLAN_ALL").is_some() {
            return 0x0F;
        }
        fn add(mask: &mut u8, n: u8) {
            if let Some(bit) = RegBank::pool_bit_of(n) {
                *mask |= bit;
            }
        }
        let mut mask = 0u8;
        for (loc, _) in self.homes.values() {
            if let Some(r) = loc.reg() {
                add(&mut mask, r.n);
            }
        }
        let Some(body) = &f.body else { return mask };
        let mut decls: Vec<&crate::ast::VarDecl> = Vec::new();
        Self::collect_local_decls(&body.items, &mut decls);
        for v in decls {
            if let Some(loc) = &v.home {
                if let Some(r) = loc.reg() {
                    add(&mut mask, r.n);
                }
            }
            if let Some(rn) = &v.register_name {
                if let Some(n) = Self::parse_reg_name(rn) {
                    add(&mut mask, n);
                }
            }
        }
        let facts = crate::ast_asm::collect_asm_facts(body);
        if facts.has_asm {
            // 含 asm ⇒ 四个池号全部预留。为什么不能只算固定约束：`q`/`Q` 是「任一
            // legacy 号」，而且一条 asm 的 `r` 操作数 + clobber 可能把 9 个
            // caller-saved 号吃光（`call_on_stack` 形态：8 clobber + 4 个 `r`
            // 操作数），规划器只能从池里补。逐句复算发射期的 owned/scratch 状态
            // 是做两份真源，而少预留的后果是**编不过**（E0311）或毁调用者。
            // 多付的上限是 (4-实用数)*8B，小于一个 16 字节对齐单位。
            mask |= 0x0F;
        }
        mask
    }

    /// 号 → 家。复用 `parse_reg_home` 的名字表，避免在这里再编一份「几号对应谁」。
    fn reg_home_location(reg: u8) -> Option<crate::location::Location> {
        const NAMES: [&str; 16] = [
            "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11",
            "r12", "r13", "r14", "r15",
        ];
        Self::parse_reg_home(NAMES.get(reg as usize)?)
    }

    pub fn compile_func(&mut self, f: &FuncDecl, prog: &Program) {
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        // 逐函数定 canary（见 `func_canary_needed`）：必须在任何发射之前算完，
        // 序言（两处）、尾声、跳表基址判据都只读这一个读数。
        self.func_canary = self.target.stack_protector && self.func_canary_needed(f);
        self.var_map.clear();
        // 段地址槽登记表是逐函数状态：不清就会把上一个函数的 `t = &gv` 广播到
        // 下一个函数的同名槽位（误发前缀 = 访存到错误的段）。
        self.local_addr_seg.clear();
        self.var_map_total_slots = 0;
        // 过对齐局部 (__attribute__((aligned(N>16))) / alignas): 需要序言把
        // %r13 动态对齐到该值 (pr16660)。变参函数暂不参与 (va_start 机构依赖
        // 固定帧顶)。栈传参函数在参数规划后再决定是否真对齐。
        let over_align = f.body.as_ref().map_or(0, |b| {
            Self::max_local_over_align(&b.items, &self.static_local_map, &f.name.name)
        });
        self.cur_func_over_align = if over_align > 16 && !f.is_variadic {
            over_align
        } else {
            0
        };
        self.cur_func_do_align = false;
        self.cur_func_anchor_off = 0;
        self.vla_offsets.clear();
        // VM 类型尺寸绑定槽是每函数帧内状态 (槽偏移在各函数独立
        // 分配), 跨函数残留会导致 sizeof 读到别的函数帧的槽
        // (20040423-1: 匿名 struct 的空 tag 在 main/sub1 同 key)
        self.vm_struct_size_slots.clear();
        // VM 成员偏移 scratch 槽池同样是每函数状态：槽偏移在各函数独立分配，
        // 残留会让下一个函数复用到别人的帧槽（读到上一个函数的 sizeof 残值）。
        self.vm_off_scratch.clear();
        self.homes.clear();
        // file-scope 绑定也要过同一道逸出判据：地址被取走的对象必须留在内存里。
        self.collect_addr_escaped(prog);
        // 并行 worker 不复用主 Codegen 的扫描结果（`compile_func` 拿到的是完整
        // `prog`，判据又只需一次 AST 遍历），所以这里也要收；主 Codegen 在
        // `compile_module` 已收过，`done` 位让它成为空操作。
        self.collect_asm_operand_names(prog);
        for (n, b) in self.global_homes.clone() {
            if !self.register_binding_allowed(&n) {
                continue;
            }
            self.homes.insert(n, b);
        }
        // 形参的家（`DCC_HOME_PARAMS=1` 时由 `ssa::regalloc::deliver` 写在 `Param::home`）。
        // 同样必须在 `resync_pins` / `reset_reg_tracker` **之前**注入：那两个函数从
        // `homes` 取号去 claim 与补序言保存，晚一步就会把刚分给形参的 callee-saved 当
        // 空闲临时号发掉（踩掉另一个变量家 = 静默错值）。
        for p in f.params.iter() {
            let (Some(nm), Some(loc)) = (p.name.as_ref(), p.home) else {
                continue;
            };
            if !self.register_binding_allowed(&nm.name) {
                continue;
            }
            self.homes.insert(nm.name.clone(), (loc, p.ty.clone()));
        }
        // 后端自选的家必须在 `resync_pins` / `reset_reg_tracker` **之前**注入：
        // 那两个函数从 `homes` 里取号去 claim 与 patch 序言保存，晚一步就会把
        // 刚分出去的 callee-saved 号当空闲临时号发掉（覆盖变量家 = 静默错值）。
        self.auto_homes.clear();
        self.plan_auto_homes(f);
        // §5.42：帧头 cs 保存槽的静态计划。必须在 `reset_reg_tracker`（序言内）
        // 与任何 `claim`/`alloc_temp` **之前**定稿：帧尺寸、变量区顶、temp 槽坐标
        // 全跟着它走，而 `RegBank` 的发号器从此刻起只在这个集合里取号。
        self.callee_plan = self.compute_callee_plan(f);
        self.reg_bank.set_plan_mask(self.callee_plan);
        // 绑定表定稿了，把钉住集合重投影一遍。**为什么现在就要做**：`naked` 分支在
        // 下面的 `reset_reg_tracker()` 之前就 `return`，而旧代码的 `owned_gp` 是当时现查
        // `reg_bindings` 算的；如果 `pinned` 等到 reset 才清，naked 函数里的内联 asm
        // 就会拿上一个函数的绑定当本函数家的名单（多排除会换发号，少排除会盖掉变量）。
        // 只同步名单，不动位图 —— 位图在 naked 路径上旧代码就是保留残值的。
        self.reg_bank.resync_pins(
            self.homes
                .values()
                .filter_map(|(l, _)| l.reg())
                .map(|r| r.n),
        );
        self.cleanup_vars.clear();
        self.sret_watermark = 0;
        // 发射站点日志也是逐函数状态：不清会把上一个函数的site 拼进本函数的 E1128，
        // 而那条诊断的全部价值就是「本函数哪几个发射站点分过槽」。
        self.sret_site_log.clear();
        self.sret_region_base = 0;
        // 帧解算表也是**逐函数**状态：`frame_size` 要等本函数的 `LayoutMap::resolve` 才算出来，
        // 在那之前 `check_frame_budget` 必须处于「尚未知帧底」而不检查的状态（`unresolved()`
        // 的 frame_size = 0，正是那个状态）。不复位就是拿**上一个函数**的帧底来量本函数
        // 规划期的分配：实测 `long_double_abi.cpp` 的 `main`（帧 1000B 左右）在规划期登记
        // `<agg_member_tmp>` 时，被上一个函数的 976B 帧判成 `E1128` 越界 —— 一条假硬错误。
        self.frame_layout = crate::codegen::x86_64::abi::layout::LayoutMap::unresolved();
        // 晚分配账面也是逐函数的（§11.23 步 2/3）。
        self.late_slot_budget = 0;
        self.late_slot_used = 0;
        self.late_var_cursor = 0;
        self.frame_planned = false;
        // 持有记录不得活过一个函数：不准则本函数开头的任何一次 `eval_held()` 会拿到
        // 上一个函数留下的号（那时代码里那个寄存器早已不是那个值）。
        self.eval_holding = None;
        // 窥孔的「上一条」同样不得跨函数：上一个函数的尾指令和本函数首指令
        // 之间隔着序言/栈帧建立，不存在任何相邻关系。
        self.peep_last = None;
        // 块内等价事实同理：上一函数末的基本块与函数边界隔开了。volatile 登记表是
        // 本帧坐标/本帧声明，泄漏到下一个函数会把别人的槽当成屏障（少削一条）或把
        // 真 volatile 槽漏掉（静默错值）—— 后者才是必须逐函数清空的原因。
        self.block_state
            .reset_at(crate::codegen::x86_64::blockopt::stat::site("reset_fn_boundary"));
        // `DCC_KILL_ALL_STAT=1`：逐函数归因，计数器在本函数开头清零。
        if crate::codegen::x86_64::blockopt::stat::enabled() {
            crate::codegen::x86_64::blockopt::stat::zero();
            crate::codegen::x86_64::blockopt::stat::zero_variants();
        }
        // Gate 0a 影子表（`DCC_SLOT_SHADOW=1`）同样逐函数配对：不清零就会把上一个函数
        // 的存槽事实当成下一个函数可转发的来源，那种读数能把 S2 的上限高估一个量级。
        if crate::codegen::x86_64::blockopt::shadow::enabled() {
            crate::codegen::x86_64::blockopt::shadow::zero();
        }
        // 上一个函数残留的冷块（嵌套函数报错等异常路径）不得混进本函数体。
        self.cold_arms.clear();
        self.cold_merged = 0;
        self.volatile_frames.clear();
        self.volatile_locals.clear();
        self.volatile_slots_overflow = false;
        self.escaped_frames.clear();
        // 本帧的控制流护栏必须在**第一条指令之前**定好：A1（「帧槽事实跨 call 存活」）在
        // 第一条 call 处就要读它。volatile 全局名集与 SSA 准入读同一个真源（§4.7）。
        self.frame_guard = crate::frame_guard::FrameGuard::for_func(
            f,
            &crate::frame_guard::volatile_global_names(prog),
        );
        // A2 的先验事实同样逐函数重算：每条标签的**入边计数**与「有没有间接跳转/内联
        // asm」都必须在本函数第一条 `label()` 之前就绪，否则那个位只能在下一个函数上生效。
        // 取交那一臂另外需要两个本函数现场累加器（seen / snap），与计数同一真源。
        let edges = crate::frame_guard::jump_edges(f);
        self.a2_incoming = edges.counts;
        self.a2_seen.clear();
        self.a2_snap.clear();
        self.a2_no_inherit = crate::frame_guard::has_unnamed_jump(f)
            // 内联 asm 单独叠一层：`%l0` 形式的标号操作数就是一条本模块数不到的边，
            // 而 `Stmt::Asm` 的文本里还可能直接 `jmp` 到一个标签上。宁关一档也不拿
            // 「看不见的边」当落空前驱用。
            || self.frame_guard.inline_asm;
        // 区域家（S4）的函数级开工判定：旋钮 + 函数级硬互斥 + `-O0`。
        //
        // `-g` 不在互斥列里，因为本层**从不推迟存出**（见 `region.rs` 头部那段推导）：
        // 内存一直是权威，单 `DW_AT_location` 就是正确答案，不需要 loclist 描述任何区间。
        // 将来谁要往本层加「内存落后于家」的形状，必须同时把区间记进
        // `Codegen::debug_loc_windows`（loclist 的生产点），并把下面这一行加回来。
        {
            let rg = &self.frame_guard;
            let body_shape_ok = !rg.blocks_region_home();
            self.region_begin_func(body_shape_ok && self.opt_level >= 1, &f.name.name);
        }
        // 前置二/三的两个函数级事实同样不得活过一个函数：覆盖表泄漏会把下一个函数的
        // 变量钉在错误的内存位置上；保存槽表则是本帧坐标，跳帧用它们是直接错值。
        self.home_overrides.clear();
        self.ext_home_saves.clear();
        // 分段位置窗口同属本帧：它们拿的是本函数的 pc 与保存槽位移，活到下一个函数
        // 就是拿别人的区间给别人发 loclist。
        self.debug_loc_windows.clear();
        self.vla_total_size = 0;
        // 静态扫描函数体是否含 VLA 声明 (决定跳转前是否恢复动态栈基)
        self.cur_func_has_vla = f
            .body
            .as_ref()
            .is_some_and(|b| Self::block_contains_vla(&b.items));
        // 跳表基址要不要常驻 %rbp（阶段 6 批 4）：与上面同一批「发射开始之前定完」的
        // 函数级事实。必须在序言之前定，因为序言要插那对存/取，而站点与尾声都读它。
        self.disptab_base_reg_sym = self.decide_disptab_base_reg(f);
        // naked 形态必须在下面那个早返**之前**就定下来：内联 asm 的保存/恢复守护
        // （P12 Phase 5.2a）不得往用户的裸汇编里插 push/pop。
        self.cur_func_is_naked = f.is_naked;
        self.current_sret_offset = None;
        // 记录当前函数名 (用于嵌套函数重命名作用域)
        self.current_func_name = f.name.name.clone();
        // static local 的可见绑定同样是逐函数状态。嵌套函数在本函数体内**就地**
        // 编译, 所以只清本函数自己那一格 —— 清整张表会把父函数已绑的名字抹掉,
        // 回到无绑定的父体内后引用就只能按名字猜了。(`static_scope_undo` 不能
        // 在这里清: 那上面还有父函数正在使用的块帧。)
        self.static_visible.remove(&self.current_func_name);
        // 记录当前函数返回类型 (用于 return 语句截断)
        self.current_func_ret_ty = f.ret_ty.clone();
        // 设置嵌套函数状态
        self.current_func_is_nested = f.is_nested;
        self.current_returns_nonnull = f.returns_nonnull;
        // noreturn: 函数末尾插入 trap 指令
        self.current_is_noreturn = f.is_noreturn;
        // 记录当前函数是否标记 no_instrument_function
        self.current_no_instrument = f.no_instrument;
        self.upvar_offsets.clear();
        // upvar_chain_depth / upvar_intermediate_offsets:
        // 嵌套函数的父级 handler 在调用 compile_func 之前通过
        // save→clear→set 预先填好这两个 map, 子 handler 负责
        // save/restore. compile_func 不再清理它们以免覆盖.
        // 顶层函数首次调用时 map 已为空 (new() 初始化), 后续
        // 顶层调用前由最后一个嵌套 handler 的 restore 清理干净.
        self.trampoline_slots.clear();
        self.trampoline_total_size = 0;
        // 收集当前函数体内定义的标签 (用于嵌套函数的 non-local goto 检测)
        self.local_labels.clear();
        // C++ 零成本异常: 受保护调用点与序言信息都是**每函数一份**; 残留上一条
        // 函数的调用点会让 LSDA 把区间登记到错误的函数 FDE 上。
        self.eh_sites.clear();
        self.cur_prologue_end = 0;
        if let Some(body) = &f.body {
            for item in &body.items {
                if let BlockItem::Stmt(s) = item {
                    Self::collect_local_labels_stmt(s, &mut self.local_labels);
                }
            }
        }
        if f.is_nested {
            for (name, off) in &f.upvars {
                self.upvar_offsets.insert(name.clone(), *off);
            }
        }
        let min_align = self
            .patchable_function_entry
            .map(|(n, _)| n.max(1))
            .unwrap_or(1);
        let func_align = f
            .alignment
            .unwrap_or(1)
            .max(min_align)
            // `-falign-functions=N`（内核 `CONFIG_FUNCTION_ALIGNMENT=y` 传 16）：gcc 靠它为
            // 每个函数发一条 `.p2align 4`，所以函数入口和包含它的自定义代码节
            // (`.noinstr.text`/`.init.text`/`.entry.text`) 的 `sh_addralign` 一起抬到 N。
            // 没传时保持 1（不影响普通构建的任何 fixture）。
            .max(self.target.align_functions.max(1) as usize)
            .max(1);
        self.text_align = self.text_align.max(func_align as u64);
        while !self.code.len().is_multiple_of(func_align) {
            self.emit_insn(&Insn::nop());
        }
        let func_start = if let Some((n, m)) = self.patchable_function_entry {
            let prefix_len = m.min(n);
            let suffix_len = n.saturating_sub(prefix_len);
            self.text_align = self.text_align.max(n.max(1) as u64);
            for _ in 0..prefix_len {
                self.emit_insn(&Insn::nop());
            }
            let func_start = self.pos();
            // 使用 asm label 覆盖符号名 (如果有)
            let sym_name = self.resolve_asm_label(&f.name.name).to_string();
            self.label(&sym_name);
            for _ in 0..suffix_len {
                self.emit_insn(&Insn::nop());
            }
            func_start
        } else {
            let func_start = self.pos();
            // 使用 asm label 覆盖符号名 (如果有)
            let sym_name = self.resolve_asm_label(&f.name.name).to_string();
            self.label(&sym_name);
            func_start
        };
        // -fcf-protection=branch|full: 函数入口（patchable 前缀 nop 之后）先发 endbr64。
        // gcc 在 `arch/x86/mm/fault.o` 里就是这个形态（16 字节 nop → 符号 → `f3 0f 1e fa`），
        // 内核 IBT 靠它把函数入口认定成合法的间接分支目标。
        if self.target.cf_protection {
            self.emit_insn(&Insn::raw_with_mnemonic(&[0xF3, 0x0F, 0x1E, 0xFA], "endbr64"));
        }
        // __attribute__((naked)): 不生成 prologue/epilogue，
        // 函数体完全由内联汇编组成，直接发射 asm 指令后返回。
        // 必须在 ftrace/prologue 之前检查，避免插入编译器生成的代码。
        if f.is_naked {
            self.compile_naked_func(f, prog, func_start);
            return;
        }
        // ftrace: -mfentry/-mrecord-mcount insert call __fentry__ before prologue
        if matches!(
            self.target.ftrace_mode,
            crate::cli::FtraceMode::FEntry | crate::cli::FtraceMode::RecordMCount
        ) {
            let call_pos = self.pos();
            self.call_label("__fentry__");
            // -mrecord-mcount: record call site address in __mcount_loc section
            if matches!(self.target.ftrace_mode, crate::cli::FtraceMode::RecordMCount) {
                self.emit_mcount_loc_entry(call_pos, &f.name.name);
            }
        }
        if self.target.frame_pointer {
            self.emit_insn(
                &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Push, 5)
                    .with_gas("push %rbp".to_string()),
            );
        }
        // 序言固定序列: push %r13; mov %rsp, %r13. 拆成两条 Insn 以便 -S 渲染各自的 gas 行.
        // 单条 5 字节发射会被 disasm 拆成两行 gas 文本, 渲染器无 gas_text 时只能 fallback.
        self.emit_insn(
            &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Push, 13)
                .with_gas("push %r13".to_string()),
        );
        self.emit_insn(
            &Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 13, 4)
                .with_gas("mov %rsp, %r13".to_string()),
        );
        if self.target.frame_pointer {
            self.emit_insn(
                &Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 5, 4)
                    .with_gas("mov %rsp, %rbp".to_string()),
            );
            // frame-pointer 模式下序言推了**两个**寄存器（rbp 与 r13），而入口时
            // rsp ≡ 8 (mod 16)，两次 8 字节之后仍就是 8 —— 本端的不变式是「体内每个 call
            // 点 rsp ≡ 0 (mod 16)」（`emit/stmt.rs` 里 pr44575 就是它的代价）。实测
            // `-fno-omit-frame-pointer` 下调用 printf("%.1f") 直接 SIGSEGV(139)，
            // 而 gcc 同一源正常。这里多下推 8 字节把它对齐；尾声用 `mov %rsp, %r13`
            // 复位栈指针，所以这 8 字节不需要（也不应该）手写回去。
            // 它必须在 `cur_prologue_end` 之前发出：FDE 的 CFA 规则从那个偏移之后才生效，
            // 落在规则之外的 prologue 差异展开器就看不到了。
            self.emit_insn(&Insn::sub_rsp_imm(8).with_gas("sub $8, %rsp".to_string()));
        }
        // C++ 零成本异常: 序言到此结束 —— FDE 的 CFA 规则必须从这一偏移之后
        // 才生效 (之前 rsp 还在变动, CFA 沿用 CIE 的 `rsp + 8`)。
        self.cur_prologue_end = self.pos() - func_start;
        self.expr_depth = 0;
        self.max_expr_depth = 0;
        self.temp_slots = 0;
        self.bf_rmw_depth = 0;
        // 序章结束时 rsp 已恢复 16 对齐 (push r13 [+ rbp] [+ 补推 8] 之后,
        // 帧 sub 的字节数也是 16 的倍数)。`stack_shift` 从这一刻起计量
        // 「相对 16 对齐基线的偏移」—— 序章里那些 push 不再计入, 否则
        // 每个调用点都会凭空多算 8 (见 call.rs 的 misalign 补偿)。
        self.stack_shift = 0;
        self.reset_reg_tracker();
        // 逃逸用的 `asm goto` clobber 掉的池号不得再当临时值发出（必须在任何发射**之前**
        // 定完，H1：第一个表达式就可能把值放进那个号）。与分配器读同一个判据。
        self.ban_escaping_asm_regs(f.body.as_ref());
        // 本函数序言/尾声补丁点在 sites 中的起始下标 (与嵌套函数隔离)
        let callee_sites_base = self.callee_save_patch_sites.len();
        if let Some(body) = &f.body {
            self.max_expr_depth = Self::max_expr_depth_stmt(&body.items);
        }
        let temp_slots = (self.max_expr_depth as i32 + 1).max(2);
        self.temp_slots = temp_slots;
        // §5.42：帧头里 callee-saved 保存槽的个数 = 计划位图的 popcount。
        // 它是帧几何的一个输入（`hdr_of(temp_slots, cs_slots)`），所以必须在
        // 第一处形参偏移计算之前就定下 —— 而计划本身在 `compile_func` 入口由
        // `compute_callee_plan` 写完（homes/局部声明的家/asm 固定约束）。
        let cs_slots = self.callee_plan.count_ones() as i64;
        // sret/物化槽最大同时存活字节水位 (与运行时分配器同口径静态扫描)
        let sret_need: i64 = f
            .body
            .as_ref()
            .map_or(0, |b| Self::max_sret_bytes(&b.items));
        // KASAN 红区: 每个局部变量后追加 32 字节 (4 个 8 字节槽位)
        let kasan_rz_slots: i32 = if self.kasan { 4 } else { 0 };
        // Static 局部变量: 已由分析 pass 预注册到 data/bss,
        // 此处仅需计算 real_global_names (排除 static local 的 mangled 名)
        // (mangled 名以 ".Lsl" 开头, 不会与 C 标识符冲突)
        self.real_global_names = self
            .globals_raw
            .keys()
            .filter(|k| !k.starts_with(".Lsl"))
            .cloned()
            .collect();
        // VLA 形参: (名字, 维度表达式, 元素大小)。`char a[2][++N]`
        // 已退化为 Ptr(Array(Char, Var(++N))), 维度表达式保留但需在
        // 序言求值 (C11 6.9.1p10: 每次调用求值, 副作用生效)
        let mut vla_param_dims: Vec<(String, Expr, usize)> = Vec::new();
        // 一维 VLA 形参被丢弃的最外层维度表达式 (Param::vla_dim):
        // 仅需在入口求值副作用, 不影响类型语义 (C11 6.9.1p10)
        let mut param_dim_effects: Vec<Expr> = Vec::new();
        // spill band 的尺寸（P12 Phase 5.1.b）：由 `pre_register_vars` 在同一个逐声明循环里
        // 从**已交付的家**（`VarDecl::home == Spill(slot)`）数出来，而不是另开一份分配或扫描。
        // 今天分配器还不产出 Spill 家 ⇒ 两个分支都是 0 ⇒ 帧与今天逐字节相同（设计 §11.4 的
        // 「band 是解算输入，不是事后发明」在这里兑现）。
        let mut spill_slots: i64 = 0;
        if f.name.name != "main" || self.relocatable {
            // 先预注册函数体中的局部变量 (使参数偏移自然在局部变量下方).
            // 使用 var_map_total_slots 累加器替代 O(n) 遍历.
            if let Some(body) = &f.body {
                // 过对齐帧: 先占掉变量区最高那一格作为「对齐前帧顶」锚点槽,
                // 变量整体下移 8 字节, 锚点落在 -(hdr) 处 (pre_register_vars 之上).
                if self.cur_func_over_align > 0 {
                    self.var_map_total_slots = 1;
                }
                Self::pre_register_vars(
                    &mut self.var_map,
                    &body.items,
                    temp_slots,
                    cs_slots as i32,
                    kasan_rz_slots,
                    &self.static_local_map,
                    &f.name.name,
                    &mut self.var_map_total_slots,
                    &mut spill_slots,
                    self.cur_func_over_align,
                );
            }
            // 参数必须落在已占用的变量区下方。占用量只准问分配器自己维护的游标
            // `var_map_total_slots`：它在 `pre_register_vars` 的同一循环里递增，已经含
            // 过对齐锚点格、逐变量对齐补齐与 KASAN 红区。以前这里是把 var_map 里的类型
            // 用 `var_alloc_slots` 重加一遍，那份「按类型重算」看不见对齐圆整吃掉的字节：
            // 一个 128 字节对齐的局部向量会让游标多走 56B，而重算值不知道，于是第一个
            // 参数落到该变量的中间，两者共用同一段栈。实测
            // gcc-torture `pr126405-3.c`（128 字节向量形参 + `v16di old = p; p = g0;`）
            // 因此把 `old` 一起清成 100，读回 0/200 而 gcc 读回 1..16。
            // limits-fndefn.c 的 100,000 参数场景仍是 O(1)，不会退回遍历。
            let mut param_used_total: i64 = self.var_map_total_slots.saturating_mul(8);
            // 注册参数到 var_map (偏移公式自动避开已注册的局部变量)
            // 记录命名参数数量 (va_start 需要)
            self.variadic_named_params =
                f.params.iter().filter(|p| p.name.is_some()).count() as i32;
            self.variadic_named_gp = 0;
            self.variadic_named_fp = 0;
            // A1: 内存返回 (sret) — 隐藏第一参数 rdi 指向调用方缓冲区.
            // 真实参数从 rsi (gp_idx=1) 开始. sret 指针保存在帧底部专用槽
            // (帧建立后计算偏移), 避免与 alloc_sret_slot 的嵌套调用临时缓冲冲突.
            let has_sret = Self::needs_memory_return(&f.ret_ty);
            if has_sret {
                self.variadic_named_gp = 1;
            }
            let mut param_stores: Vec<(u8, i64)> = Vec::new(); // (gp reg, offset)
                                                               // 形参有 callee-saved 家时，入站值直接搬进那个号而不是存槽。
                                                               // 只出现在 `DCC_HOME_PARAMS=1` 且该形参确实拿到家时；默认表为空。
            let mut param_home_moves: Vec<(u8, u8)> = Vec::new(); // (入站号, 家号)
                                                                  // (xmm reg, offset, ty, wide128): wide128=true 表示该 Sse chunk
                                                                  // 与紧随的 SseUp 共用同一 xmm, 一次性保存完整 16 字节。
            let mut xmm_param_stores: Vec<(u8, i64, Type, bool)> = Vec::new();
            // (栈上字节偏移, var offset)。SysV 栈参数按类型对齐:
            // long double / __int128 / 16 字节对齐聚合体须落在 16 的
            // 倍数上, 否则 callee 读取位置与 caller 布局错位 (pr44942)。
            let mut stack_param_stores: Vec<(i64, i64)> = Vec::new();
            // D10-2: 栈传的具名 `long double` 形参单独登记：它们不能逐字搬运，
            // 必须在入口处把 SysV 的 80-bit 编码降级成 dcc 内部表示（见消费端）。
            let mut ld_param_stores: Vec<(i64, i64)> = Vec::new();
            // `_Complex long double` 具名形参: (入站 real 偏移, 局部 real 偏移)。
            // imag 紧随各自 real +16。入口处用 fldt/fstpt 逐分量拷贝 (见 Phase 2)。
            let mut cld_param_stores: Vec<(i64, i64)> = Vec::new();
            let mut gp_idx = if has_sret { 1 } else { 0 };
            let mut fp_idx = 0usize;
            let mut stack_arg_off: i64 = 0;
            for p in f.params.iter() {
                let nm_opt = p.name.as_ref();
                // VLA 形参检测: 记录维度表达式, 序言中求值并存 stride 槽
                if let Some(nm) = nm_opt {
                    if let Type::Ptr(q) = &p.ty {
                        if let Type::Array(elem, ArraySize::Var(se)) = &q.base {
                            vla_param_dims.push((
                                nm.name.clone(),
                                (**se).clone(),
                                elem.base.size(),
                            ));
                        }
                    }
                    if let Some(se) = &p.vla_dim {
                        param_dim_effects.push((**se).clone());
                    }
                }
                // __int128 / 聚合体按 8 字节块占用 GP 寄存器或栈槽
                // 所有 struct/union/array 均走聚合路径, 确保 SSE 类
                // 小结构体 (<=8B) 也能正确从 XMM 寄存器接收.
                //
                // 关键: gp_idx / fp_idx / stack_arg_off / param_used_total
                // 必须在有/无名字参数上**一致推进**, 因为 SysV ABI 的
                // 寄存器分配不看参数是否有名字 — 无名字参数 (如原型
                // `f(uint32_t, int32_t *r)`) 也要占 rdi, 后续有名字参数
                // 才会落到正确的寄存器 (rsi). 之前这些推进逻辑全部包
                // 在 `if let Some(nm)` 里, 导致 pr103209 / simple7 中
                // 有名字参数被错误地分配到前一个无名字参数应占据的寄
                // 存器, 栈槽里是垃圾值.
                let is_agg_param = matches!(&p.ty, Type::Int128 | Type::UInt128 | Type::Complex(_))
                    || matches!(
                        &p.ty,
                        Type::Struct(_, _, _, _)
                            | Type::Union(_, _, _, _)
                            | Type::Array(_, ArraySize::Const(_))
                            | Type::Atomic(_)
                            | Type::Vector(_, _)
                    );
                // complex LD 具名形参: 整体走内存, 两个 16 字节栈槽。
                // 与 caller emit_sysv_call 的规划保持一致, 不能进通用聚合
                // 8 字节 chunk 路径 (会破坏 tbyte)。
                let is_cld_param = matches!(&p.ty, Type::Complex(inner)
                    if matches!(&inner.base, Type::LongDouble));
                if is_cld_param {
                    let used_before = param_used_total;
                    param_used_total += 32;
                    let aligned = (stack_arg_off + 15) & !15;
                    stack_arg_off = aligned + 32;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                        // 32 字节 (real@off, imag@off+16) 在帧槽中占 4 个
                        // 8 字节槽。局部槽公式与标量 LD (16B→-(SIZE-8)=-8)
                        // 及通用聚合 base=-(u+hdr)-(chunks-1)*8 一致:
                        // SIZE=32 → -(32-8)=-24。误用 -16 会让 imag 槽顶 2
                        // 字节与相邻 GP 参数槽重叠 (tag qword 覆盖 imag
                        // 指数字节, complex LD 后随的指针/整数参数被踩坏)。
                        let off = -(used_before + hdr) - 24;
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        cld_param_stores.push((aligned, off));
                    }
                } else if is_agg_param {
                    let classes = Self::abi_classes(&p.ty);
                    let chunks = p.ty.size().div_ceil(8);
                    let need_gp = classes.iter().filter(|c| **c == AbiClass::Integer).count();
                    // SseUp 是前一个 Sse 所在 xmm 高 64 位的延续, 不单独占用
                    // 寄存器, 故寄存器计数只数 Sse (SysV ABI 3.2.3)。
                    let need_fp = classes.iter().filter(|c| **c == AbiClass::Sse).count();
                    let agg_align = (p.ty.align() as i64).max(8);
                    let used_before = param_used_total;
                    param_used_total += (p.ty.size().div_ceil(8) * 8) as i64;
                    // SysV ABI: 聚合参数可以部分通过寄存器、部分通过栈传递。
                    // 例如 _Complex double 在只有 1 个 XMM 可用时，实部用寄存器，
                    // 虚部通过栈传递。必须逐个 chunk 处理，不能整体判断。
                    let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                    let base_off = -(used_before + hdr) - (chunks as i64 - 1) * 8;
                    if let Some(nm) = nm_opt {
                        self.var_map
                            .insert(nm.name.clone(), (base_off, p.ty.clone()));
                    }
                    let mut local_gp = gp_idx;
                    let mut local_fp = fp_idx;
                    // SysV 3.5.1 all-or-nothing: 任一 eightbyte 放不进剩余寄存器,
                    // 整个对象一次性走 overflow 区。必须与 caller `emit_sysv_call`
                    // 共用同一真源 (aggregate_forces_memory), 否则两侧对同一聚合
                    // 得出不同的拆分方案: caller 整体入栈、callee 却按"低半进
                    // r9 + 高半进栈"取参, 被调方读到的低半是垃圾。
                    let force_mem_agg =
                        Self::aggregate_forces_memory(&classes, 6 - local_gp, 8 - local_fp);
                    // 上一个 Sse chunk 的位置, SseUp 复用之 (同一 xmm 的高半)。
                    // Some((reg, off)): 在寄存器; Some((None, off)): 在栈。
                    let mut prev_sse: Option<(Option<u8>, i64)> = None;
                    for c in 0..chunks {
                        let raw_cls = classes.get(c).copied().unwrap_or(AbiClass::Memory);
                        // 整体走内存时把首 chunk 的分类改写成 Memory, 由下方
                        // Memory 分支一次性排好 chunks 个 overflow 槽并 break。
                        let cls = if force_mem_agg { AbiClass::Memory } else { raw_cls };
                        let chunk_off = base_off + c as i64 * 8;
                        match cls {
                            AbiClass::Integer => {
                                if local_gp < 6 {
                                    // 通过 GP 寄存器传递
                                    if nm_opt.is_some() {
                                        param_stores.push((arg_regs[local_gp], chunk_off));
                                        self.variadic_named_gp += 1;
                                    }
                                    local_gp += 1;
                                } else {
                                    // GP 寄存器不足，通过栈传递
                                    let aligned =
                                        (stack_arg_off + agg_align - 1) & !(agg_align - 1);
                                    stack_arg_off = aligned + 8;
                                    if nm_opt.is_some() {
                                        stack_param_stores.push((aligned, chunk_off));
                                        self.variadic_named_gp += 1;
                                    }
                                }
                            },
                            AbiClass::Sse => {
                                // 下一 chunk 是否为 SseUp: 若是, 本 Sse 与它共用
                                // 同一 xmm, 标记 wide128 一次性保存 16 字节。
                                let next_up = classes.get(c + 1).copied() == Some(AbiClass::SseUp);
                                if local_fp < 8 {
                                    // 通过 XMM 寄存器传递。仅当类型真正占满
                                    // 16 字节 (而非 16B 对齐但只含 8B double
                                    // 的 aligned_d) 时, 才与 SseUp 合并做一次
                                    // 128 位保存, 否则高半是相邻栈槽, 会被覆盖。
                                    let wide128 = next_up && p.ty.size() == 16;
                                    if nm_opt.is_some() {
                                        xmm_param_stores.push((
                                            local_fp as u8,
                                            chunk_off,
                                            p.ty.clone(),
                                            wide128,
                                        ));
                                        self.variadic_named_fp += 1;
                                    }
                                    prev_sse = Some((Some(local_fp as u8), chunk_off));
                                    local_fp += 1;
                                } else {
                                    // XMM 寄存器不足，通过栈传递
                                    let aligned =
                                        (stack_arg_off + agg_align - 1) & !(agg_align - 1);
                                    stack_arg_off = aligned + 8;
                                    if nm_opt.is_some() {
                                        stack_param_stores.push((aligned, chunk_off));
                                        self.variadic_named_fp += 1;
                                    }
                                    prev_sse = Some((None, aligned));
                                }
                            },
                            AbiClass::SseUp => {
                                // SseUp 不占新寄存器: 它复用前一个 Sse 的同一
                                // xmm 的高 64 位。寄存器情形已由 Sse 以 wide128
                                // 一次性 movups 保存, 这里不再推进 fp、不发 store;
                                // 仅当 Sse 落在栈上时, 高 8 字节位于相邻栈槽。
                                match prev_sse {
                                    Some((Some(_), _)) => {
                                        // 寄存器: 无需额外动作 (wide128 已覆盖)
                                    },
                                    Some((None, prev_aligned)) => {
                                        if nm_opt.is_some() {
                                            stack_param_stores.push((prev_aligned + 8, chunk_off));
                                        }
                                        stack_arg_off = prev_aligned + 16;
                                    },
                                    None => {},
                                }
                                // 计数需要与 caller 一致: SseUp 不增加已用 xmm。
                            },
                            AbiClass::Memory
                            | AbiClass::X87
                            | AbiClass::X87Up
                            | AbiClass::ComplexX87 => {
                                // 强制内存类：整个参数通过栈传递
                                let aligned = (stack_arg_off + agg_align - 1) & !(agg_align - 1);
                                stack_arg_off = aligned + chunks as i64 * 8;
                                if nm_opt.is_some() {
                                    for i in 0..chunks {
                                        let off = base_off + i as i64 * 8;
                                        stack_param_stores.push((aligned + i as i64 * 8, off));
                                    }
                                    self.variadic_named_gp += need_gp as i32;
                                    self.variadic_named_fp += need_fp as i32;
                                }
                                break; // 所有 chunks 都已处理
                            },
                            AbiClass::NoClass => unreachable!(),
                        }
                    }
                    // 更新全局索引
                    gp_idx = local_gp;
                    fp_idx = local_fp;
                } else if matches!(&p.ty, Type::LongDouble) && self.target.sse {
                    let used_before = param_used_total;
                    param_used_total += 16;
                    let aligned = (stack_arg_off + 15) & !15;
                    stack_arg_off = aligned + 16;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                        let off = -(used_before + hdr) - 8;
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        // 不走逐字 8+8 搬运：改由 ld_param_stores 在入口降级。
                        ld_param_stores.push((aligned, off));
                    }
                } else if matches!(&p.ty, Type::Float128) && self.target.sse {
                    // binary128 标量：占 16 字节栈槽但只消耗**一个** XMM 寄存器
                    // （`[Sse, SseUp]`，SseUp 是前一 SSE 的高半）。gcc -O0 实测
                    // `f(__float128 a, __float128 b)` 装 a→xmm0、b→xmm1。
                    //
                    // 帧槽必须**下移 8 字节**：标量槽公式 `off = -(used_before+hdr)`
                    // 让槽从 `off` 往上长 `size` 字节，而 16 字节的槽若不额外下移，
                    // 就会与前一个参数的 8 字节槽重叠 —— 前者的 `mov %rdi,off` 会
                    // 盖掉 binary128 的高 8 字节，callee 读回的是半截值
                    // （实测 `void d(const char*, __float128)` 直接段错误：
                    //  `n` 的槽尾被 v 的高半覆盖，`printf("%s", n)` 拿到垃圾指针）。
                    // 16 字节聚合体走的是同一个约定（见上面的
                    // `base_off = -(used_before+hdr) - (chunks-1)*8`）。
                    let used_before = param_used_total;
                    param_used_total += 16;
                    if fp_idx < 8 {
                        let saved_fp = fp_idx;
                        fp_idx += 1;
                        if let Some(nm) = nm_opt {
                            let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                            let off = -(used_before + hdr) - 8;
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            // wide128=true ⇒ 入口用 movups 保存完整 16 字节。
                            xmm_param_stores.push((saved_fp as u8, off, p.ty.clone(), true));
                            self.variadic_named_fp += 1;
                        }
                    } else {
                        // XMM 耗尽：按 16 字节对齐走栈，分两个 8 字节搬运。
                        let aligned = (stack_arg_off + 15) & !15;
                        stack_arg_off = aligned + 16;
                        if let Some(nm) = nm_opt {
                            let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                            let off = -(used_before + hdr) - 8;
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            stack_param_stores.push((aligned, off));
                            stack_param_stores.push((aligned + 8, off + 8));
                            self.variadic_named_fp += 1;
                        }
                    }
                } else if matches!(&p.ty, Type::Float | Type::Double) && self.target.sse {
                    let used_before = param_used_total;
                    param_used_total += 8;
                    if fp_idx < 8 {
                        let saved_fp = fp_idx;
                        fp_idx += 1;
                        if let Some(nm) = nm_opt {
                            let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                            let off = -(used_before + hdr);
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            xmm_param_stores.push((saved_fp as u8, off, p.ty.clone(), false));
                            self.variadic_named_fp += 1;
                        }
                    } else {
                        let my_stack_off = stack_arg_off;
                        stack_arg_off += 8;
                        if let Some(nm) = nm_opt {
                            let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                            let off = -(used_before + hdr);
                            self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                            stack_param_stores.push((my_stack_off, off));
                            self.variadic_named_fp += 1;
                        }
                    }
                } else if gp_idx < 6 {
                    let used_before = param_used_total;
                    let reg = arg_regs[gp_idx];
                    param_used_total += 8;
                    gp_idx += 1;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                        let off = -(used_before + hdr);
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        // 家有号 ⇒ 入站值直接搬到家号，不写帧槽。两边都是 GP 号且
                        // 家限定在 callee-saved（与入站号不相交），所以这里不可能出现
                        // 「后面的形参从已被覆写的入站号里读」那种顺移错误。
                        match self.homes.get(&nm.name).map(|(l, _)| *l) {
                            Some(crate::location::Location::Reg(rid))
                                if rid.bank == crate::location::RegBank::Gp && rid.n != reg =>
                            {
                                param_home_moves.push((reg, rid.n));
                            },
                            // 家恰好就是入站号（参数位置与家重合）：什么都不用发。
                            Some(crate::location::Location::Reg(rid))
                                if rid.bank == crate::location::RegBank::Gp => {},
                            _ => param_stores.push((reg, off)),
                        }
                        self.variadic_named_gp += 1;
                    }
                } else {
                    let used_before = param_used_total;
                    param_used_total += 8;
                    let my_stack_off = stack_arg_off;
                    stack_arg_off += 8;
                    if let Some(nm) = nm_opt {
                        let hdr = LayoutMap::hdr_of(temp_slots as i64, cs_slots);
                        let off = -(used_before + hdr);
                        self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                        stack_param_stores.push((my_stack_off, off));
                        self.variadic_named_gp += 1;
                    }
                }
            }
            // 记录栈传参数量 (va_start overflow_arg_area 需要):
            // overflow_arg_area 指向最后一个命名栈参数之后,
            // 与 caller 的对齐布局一致
            self.variadic_overflow_offset = 16 + stack_arg_off;
            // 将参数占用的栈空间计入 var_map_total_slots,
            // 使后续 register_var 运行时分配 (如 compound literal 临时变量)
            // 不会与参数栈槽重叠 (根因: compound lit temp 分配到参数 s 的
            // 栈位导致 pr114207.c 等测试 segfault/abort)
            //
            // 赋值而不是累加（报告 §5.39-d）：`param_used_total` 的初值就是
            // `var_map_total_slots * 8`（局部变量区已占字节），每个形参再往上加 ——
            // 它本身就是「局部 + 形参」的**总数**。以前写成
            // `var_map_total_slots += param_used_total / 8`，于是局部变量被数了两遍，
            // 变量区大约翻倍：实测 `__schedule` 的 37 槽局部 + 1 个形参被算成 76 槽
            // （608 B，而帧里真正用到的只有 296 B）。
            let param_slots = param_used_total / 8;
            self.var_map_total_slots = param_slots;
            // 与上面同一根因、同一条轴: 复合字面量 / VM 尺寸元信息等只在代码生成
            // 时才 `register_var` 的对象现在有自己的一条带（§11.23 步 3），它的尺寸就是
            // 本函数在这里定下的 `late_slot_budget`，没有再挤变量区。
            self.plan_late_slot_budget(f.body.as_ref(), &f.params, &f.ret_ty);
            // 帧布局**解算**（P12 Phase 2）：以前这里是一段现场求和，现在只保留
            // 输入；所有带边界由 `abi::layout::LayoutMap::resolve` 一次算完。
            // 求和项与求和顺序与搬迁前字面一致（`var_bytes + hdr + sret_need +
            // sc_slot + cx_band + va_slots + vec_reserve + bf_rmw_reserve +
            // tramp_reserve + stack_reserve`，再 16 对齐）。
            let var_bytes: i64 = self.var_map_total_slots.saturating_mul(8);
            if std::env::var_os("DCC_SLOT_HOME").is_some() {
                eprintln!(
                    "VARBYTES {} slots={} param_used={} params={}",
                    f.name.name,
                    self.var_map_total_slots,
                    param_used_total,
                    f.params.len()
                );
            }
            let sc_slot: i64 = if f.is_nested { 8 } else { 0 }; // 嵌套函数需要静态链槽位
                                                                // P3 (honest reservation): 有多少直接嵌套函数就需要多少个 32B trampoline。
                                                                // 之前固定 256B 意味着无嵌套函数每帧白开 256B；现在从「全函数统一」
                                                                // 变成「按 body.items 里直接嵌套函数个数 × 32B」。
            let mut p3_nested_count: i64 = 0;
            if let Some(body) = &f.body {
                let mut collected: Vec<crate::ast::FuncDecl> = Vec::new();
                Self::collect_direct_nested_funcs(&body.items, &mut collected);
                p3_nested_count = collected.len() as i64;
            }
            let tramp_reserve_honest: i64 = p3_nested_count.saturating_mul(32);
            // bf_rmw 带也改成**按需**（§5.12）：尺寸 = 本函数最深那条位域 RMW 链的层数
            // × 32B，判据与发射器同源（`Codegen::bf_rmw_chain_depth`）。这一步同时修掉了
            // 一个旧坑：恒 64B 只容 2 层，而 `bitfield_rmw_deep_nest.c` 的
            // `s.a = (s.b = (s.c = …))` 是 9 层，它一直在往带外写 224B，只是外面碰巧是
            // 恒预留的富余。trampoline / complex / bitfield 三条带现在各占自己的区间，
            // 见 `abi/layout.rs` 里 `bf_rmw_band_top` / `tramp_top` 的推导。
            let bf_rmw_reserve_honest: i64 =
                crate::codegen::x86_64::abi::layout::bf_rmw_band_reserve(self.bf_rmw_max_depth);
            let cx_band: i64 = crate::codegen::x86_64::abi::layout::cx_band_reserve(self.needs_cx_band);
            let vec_reserve_honest: i64 =
                crate::codegen::x86_64::abi::layout::vec_band_reserve(self.max_vec_bytes);
            let va_slots: i64 = if f.is_variadic {
                if self.target.sse {
                    48 + 128
                } else {
                    48
                }
            } else {
                0
            };
            // spill band：尺寸来自上面那个逐声明循环（`spill_slots`）。Phase 5.1 今天
            // 分配器不产出 Spill 家 ⇒ 它仍是 0，但band 已经是布局解算的**输入**，所以
            // 打开溢出时不会再长出第七个推算者。
            // 过对齐帧: 仅当「非变参 + 无栈传参」才真把 %r13 动态对齐下来.
            // 栈传参走 [r13+16] 寻址 (帧顶之上), r13 一下移就会错位, 故这类
            // 函数退回不对齐 (过对齐局部退化为现状, 不引入新错误).
            if self.cur_func_over_align > 0
                && stack_param_stores.is_empty()
                && ld_param_stores.is_empty()
                && cld_param_stores.is_empty()
            {
                self.cur_func_do_align = true;
                self.cur_func_anchor_off =
                    -crate::codegen::x86_64::abi::layout::LayoutMap::hdr_of(
                        temp_slots as i64,
                        cs_slots,
                    );
            }
            let plan = crate::codegen::x86_64::abi::layout::FramePlan {
                    temp_slots: temp_slots as i64,
                    cs_slots,
                    var_bytes,
                    // 晚分配带（§11.23 步 3）：尺寸就是规划期那份可枚举预算。
                    // 带为 0 的函数（一个发射期对象也没有）仍与今天逐字节相同。
                    late_slots: self.late_slot_budget,
                    sret_need,
                    va_slots,
                    static_chain_slot: sc_slot,
                    bf_rmw_reserve: bf_rmw_reserve_honest,
                    cx_band,
                    vec_reserve: vec_reserve_honest,
                    tramp_reserve: tramp_reserve_honest,
                    stack_reserve: self.stack_reserve_bytes(),
                    spill_slots,
                };
            self.frame_dump(&f.name.name, &plan);
            let layout = crate::codegen::x86_64::abi::layout::LayoutMap::resolve(&plan);
            self.frame_layout = layout;
            // sret/物化槽区顶界：紧邻变量区底（变量区占 -(hdr+var_bytes)）。
            // 这一条**生成期会变**（`register_var` 会把它跟变量区一起往下压），
            // 所以它留在 Codegen 当游标；而 `frame_size` / `vec_temp_off` 解算后不再变，
            // 镜像字段已删，花费者一律直读 `LayoutMap`（帧的坐标从此只有一份）。
            self.sret_region_base = layout.sret_region_base;
            // 过对齐帧: 先把 %r13/%rsp 对齐到局部最大对齐值. 必须在 sub_rsp
            // 之前, 这样 sub 出来的帧底落在对齐基线之下, 被调帧不会盖到本帧.
            self.emit_over_align_base();
            if self.frame_layout.frame_size > 0 {
                if self.target.stack_clash_protection && self.frame_layout.frame_size > 4096 {
                    self.emit_stack_clash_probes(self.frame_layout.frame_size);
                } else {
                    self.emit_sub_rsp(self.frame_layout.frame_size);
                }
            }
            // 帧已分配 ⇒ 此刻 rsp 正落在帧线上，从这里开始记「体内比帧线低多少」的账
            // （尾声据此决定发 `add $帧长,%rsp` 还是退回 `mov %r13,%rsp`）。
            self.arm_rsp_ledger();
            // -ftrivial-auto-var-init=zero: 零初始化整个栈帧. 对超大帧跳过.
            if self.target.auto_var_init_zero
                && self.frame_layout.frame_size > 0
                && self.frame_layout.frame_size <= 16 * 1024 * 1024
            {
                self.emit_zero_init_frame(self.frame_layout.frame_size);
            }
            // 过对齐帧: 把对齐前的帧顶写进锚点槽 (供尾声恢复 rsp). 须在 sub 之后,
            // 且**必须晚于零初始化**: 零初始化覆盖 `[r13-frame_size, r13)`，锚点槽
            // （`-hdr_of(temp_slots)`，实测 -0x50）正落在这一段里 —— 先写锚点再清帧
            // 会把「对齐前的帧顶」清成 0，尾声 `mov -0x50(%r13),%rsp` 于是把 rsp
            // 置 0，`pop %r13` 当场 #PF→#DF（kbuild14 boot 的停点）。
            // emit_zero_init_frame 自行 push/pop rax，对齐前的帧顶跨它存活。
            self.emit_over_align_anchor();
            // Stack canary: 在帧顶部 (r13-8) 存储金丝雀值
            if self.func_canary && self.frame_layout.canary_fits() {
                // mov rax, <guard> → 加载 canary（guard 的寻址形态见 `emit_canary_load`）
                self.emit_canary_load();
                // mov [r13 - 8], rax  → 存储在帧顶部
                self.emit_insn(&Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    13,
                    -8,
                ));
            }
            // KASAN/ASan 栈插桩：进函数先清整帧的陈旧毒，再只毒真实红区。
            // 旧写法「毒整帧 + 只解具名变量」会让晚分配带（发射期才登记）、spill 槽与
            // 对象尾部终身带毒，未插桩库正常写入就报 use-after-poison 假阳性
            // （判据本体见 `special/sanitizer.rs::emit_kasan_stack_poison_redzones`）。
            if self.kasan && self.frame_layout.frame_size > 0 {
                self.emit_kasan_stack_poison_redzones();
            }
            // 被调用者保存寄存器: 只保存**计划里有的**号（§5.42 按需化）。
            //
            // 旧口径无条件发 4 条、函数体发完后再把没用到的改 nop，而帧槽恒占 32B ——
            // 内核 trace 上 119 帧平均只真用 1.11 个号，白付的槽直接吃任务栈余量。
            // 现在帧头的槽数 = plan 的 popcount，第 j 个被规划的号落在 -(16+8j)；
            // 发射顺序仍是 rbx/r12/r14/r15（位序即产物，换序就是换产物）。
            //
            // 补丁点只在**真发了字节**时登记：`emit_insn` 可能因块内等价事实而整条
            // 不发（P13 阶段6），而 `patch_callee_saves` 是按「起点 + 4 字节」写 nop 的，
            // 一条没发的指令留下一个陈旧起点就会把后面 4 字节（可能是 `mov %r13,%rsp`
            // 的前半）涂成 nop —— 实测症状是尾声只剩 `pop %rbp; ret`，SIGSEGV。
            let mut cs_j: i64 = 0;
            for (bit, reg, name) in [
                (0x01u8, 3u8, "rbx"),
                (0x02, 12, "r12"),
                (0x04, 14, "r14"),
                (0x08, 15, "r15"),
            ] {
                if self.callee_plan & bit == 0 {
                    continue;
                }
                let off = -(16 + cs_j * 8) as i32;
                Self::emit_callee_save_site(
                    self,
                    bit,
                    Insn::mov_rm_mem(
                        false,
                        crate::codegen::shared::asm_operand::Width::B64,
                        reg,
                        13,
                        off,
                    )
                    .with_gas(format!("mov %{name}, {off}(%r13)")),
                );
                cs_j += 1;
            }
            // 跳表基址常驻 `%rbp`（阶段 6 批 4）：先把调用者的 `%rbp` 存进帧顶那一格，
            // 再把表地址取一次。存槽与金丝雀共用 `%r13-8`，两者由
            // `decide_disptab_base_reg` 的互斥判据分开，所以下面必须先问「帧里放得下吗」
            // ——放不下就整体退出（把字段清成 None），发射站点与尾声都跟着回到原形状。
            //
            // 不在 `patch_callee_saves` 的位图里：那四条是「可能本函数根本不用」的号，
            // 而这一条用不用在序言这一刻已经确定（AST 扫描完），不需要回填。
            if self.disptab_base_reg_sym.is_some() {
                if !self.frame_layout.canary_fits() {
                    self.disptab_base_reg_sym = None;
                } else {
                    let sym = self.disptab_base_reg_sym.clone().unwrap();
                    self.emit_insn(
                        &Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            5,
                            13,
                            -8,
                        )
                        .with_gas("mov %rbp, -8(%r13)".to_string()),
                    );
                    // lea <表>(%rip), %rbp：与 `emit_global_addr` 同一条重定位通道
                    //（传入的已是 mangled 名，它内部的 `resolve_global_key` 对原名字是恒等）。
                    self.emit_global_addr(5, &sym);
                }
            }
            // 变参 reg save area 放在 frame 底部 (紧贴 rsp 之后).
            // 帧布局 (自下而上, §5.42):
            //   r13+0 saved r13 / r13+8 ret addr
            //   r13-8 canary / 跳表基址槽
            //   r13-16..-(16+8*cs) callee-saved 保存区 (仅计划内的号, 紧凑编号)
            //   往下 temp_slots 个 binop 槽 (槽 d 在 -(16+8*cs+8*d))
            //   再往下 var_bytes 变量区（帧头 hdr = 16 + 8*cs + temp_slots*8；
            //   `+16` 的第二个 8 就是变量区第一槽，可参见 `LayoutMap::hdr_of`）
            //   r13-frame_size..r13-frame_size+cx_band 复数 rvalue 物化带 (见 CX_FRAME_BAND)
            //   r13-frame_size+cx_band..+cx_band+va_slots 变参 reg save area
            // 关键: save area 必须在变量下方, 否则 XMM 槽位会与
            //       变量 (尤其 for-init 的 i) 重叠, 导致变参读取
            //       错误返回循环变量值. 而 save area 下方还要留出复数物化带,
            //       否则复数内存对会写坏 GP 槽 1 (保存的 rsi), 使后续
            //       `va_arg` 从 gp_offset=8 读回复数位模式。
            self.variadic_reg_save_offset = self.frame_layout.variadic_reg_save_off;
            if f.is_variadic {
                for (i, reg) in arg_regs.iter().enumerate() {
                    self.store_stack(*reg, self.variadic_reg_save_offset + i as i64 * 8);
                }
                if self.target.sse {
                    for i in 0..8u8 {
                        self.store_xmm128_stack(
                            i,
                            self.variadic_reg_save_offset + 48 + i as i64 * 16,
                        );
                    }
                }
            }
            // 嵌套函数: 保存静态链. 调用方约定以 r10 传入父帧指针
            // (gen_call: mov r10, r13; trampoline: mov r10, r13), 序言必须
            // 保存同一寄存器. 注意不能用 rdx: rdx 是第 3 个整型参数寄存器,
            // 嵌套函数有 ≥3 个整型参数时静态链会覆盖第 3 个参数.
            if f.is_nested {
                self.static_chain_offset = self.frame_layout.static_chain_off;
                // mov [r13 + static_chain_offset], r10
                self.store_stack(10, self.static_chain_offset);
                // 登记本帧链槽偏移: 后代嵌套函数多层上溯静态链时逐帧使用
                self.nested_chain_offsets
                    .insert(self.current_func_name.clone(), self.static_chain_offset);
            }
            // ftrace: -pg inserts call mcount after prologue
            if matches!(self.target.ftrace_mode, crate::cli::FtraceMode::MCount) {
                self.call_label("mcount");
            }
            // Phase 2: 帧已建立 (r13 已设置), 现在存储参数到栈槽
            for (reg, off) in param_stores {
                self.store_stack(reg, off);
            }
            // 入站值 → 家号。上面的 callee-saved 保存已经完成（序言开头），现在写
            // 它们不会丢任何东西；与存栏同一批次，不依赖顺序（号集不相交）。
            for (incoming, home) in param_home_moves {
                self.mov(home, incoming);
            }
            for (xmm, off, ty, wide128) in xmm_param_stores {
                if matches!(ty, Type::Float) {
                    // K&R / 无原型函数: float 参数经默认提升后以 double 传入.
                    // callee 必须先将 xmm 中的 double 转为 float 再存储.
                    if f.is_old_style {
                        // cvtsd2ss xmmN, xmmN: double → float
                        // 两个 ModRM 字段都是同一个号 ⇒ xmm>=8 必须同时置 REX.R 与 REX.B。
                        // 旧写法先算 `let lo = xmm & 7` 再拿 `lo >= 8` 判扩张，而掩蔽后的
                        // 值永远 <8，于是 xmm 落在 8..15 时两个字段都编成了低 8 号
                        // （写的不是 %xmm8-15而是 %xmm0-7）。
                        self.emit_insn(&Insn::raw_bytes(&[0xF2]));
                        if xmm >= 8 {
                            self.emit_insn(&Insn::raw_bytes(&[0x45])); // REX.R+B，同一个号既在 reg 又在 r/m
                        }
                        self.emit_insn(&Insn::raw_bytes(&[
                            0x0F,
                            0x5A,
                            0xC0 | ((xmm & 7) << 3) | (xmm & 7),
                        ]));
                    }
                    self.store_xmm32_stack(xmm, off);
                } else if wide128 || (ty.size() == 16 && matches!(ty, Type::Vector(_, _))) {
                    // 128-bit vector 参数 (如 __int128 vector), 或聚合中
                    // [Sse, SseUp] 共用同一 xmm 的 16 字节 (如含向量字段的
                    // 结构体): 保存完整 16 字节。
                    self.store_xmm128_stack(xmm, off);
                } else {
                    self.store_xmm64_stack(xmm, off);
                }
            }
            for (arg_off, off) in ld_param_stores {
                // D10-2（已简化）：SysV AMD64 下 x87 **参数走内存** —— 每个具名
                // `long double` 占一个 16 字节栈槽，内容是调用方 `fstpt` 写出的
                // **真 80-bit 编码**（gcc callee 就是 `fldt 0x10(%rbp)` 读它）。
                //
                // 旧实现在此做 `fldt` → `fstp qword` → `movq $0, off+8` 的“入口
                // 降级一次”，目的是维持“局部槽低 8 字节 = f64 位型、高 8 字节 = 0”
                // 这个旧不变量。现在对象表示与 rvalue 值通道都已是真 80-bit，那次
                // 降级反而**把调用方送来的 64 位尾数舍回 53 位**（并且 `fstp qword`
                // 不写指数字节，配合槽高位清零会把值读成 0）。故改为槽→槽的
                // `fldt`/`fstpt` 直拷，不再有任何精度损失。
                let incoming = 16i64 + arg_off;
                // P11 Bucket AR: typed x87 fldt/fstpt via emit_x87_fld_at/fstp_at (base=r13).
                self.emit_x87_fld_at(13, incoming as i32, &Type::LongDouble);
                self.emit_x87_fstp_at(13, off as i32, &Type::LongDouble);
            }
            for (arg_off, off) in cld_param_stores {
                // complex LD: 两个 16 字节栈槽, 逐分量 fldt/fstpt 直拷
                // (real@arg_off→off, imag@arg_off+16→off+16), 无精度损失。
                for k in 0..2 {
                    let incoming = 16i64 + arg_off + k * 16;
                    let dst = off + k * 16;
                    self.emit_x87_fld_at(13, incoming as i32, &Type::LongDouble);
                    self.emit_x87_fstp_at(13, dst as i32, &Type::LongDouble);
                }
            }
            // 连续 chunk 的栈参数落位：逐 chunk 的 `load; store` 对 256KB 的
            // by-value 聚合参数是 65536 条指令（pr28982b 的 `bar`，目标文件
            // 696KB vs gcc 5.2KB）。把 (源, 目标) 同步递增的区间合并，>=128B
            // 的用 `rep movsb` 整块搬运。源在 r13 上方（调用方栈）、目标在帧内，
            // 两者不可能重叠。
            {
                let mut i = 0usize;
                while i < stack_param_stores.len() {
                    let (a0, o0) = stack_param_stores[i];
                    let delta = a0 - o0;
                    let mut n = 1usize;
                    while i + n < stack_param_stores.len() {
                        let (a, o) = stack_param_stores[i + n];
                        let k = n as i64;
                        if a - o == delta && o == o0 + k * 8 {
                            n += 1;
                        } else {
                            break;
                        }
                    }
                    let bytes = n as i64 * 8;
                    if bytes >= 128 {
                        // rdi 可能还是尚未落位的 sret 指针（它在本段之后才保存），
                        // rcx/rsi 名义上是 caller-saved，但同批参数落位仍以
                        // 不改写任何入站寄存器为准。
                        for r in [1u8, 6, 7] {
                            self.emit_insn(&Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                                    op: crate::codegen::x86_64::opcode::StackOp::Push,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        r,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            ));
                        }
                        self.mov_imm(1, bytes as i128);
                        self.emit_lea_r13(6, 16 + a0);
                        self.emit_lea_r13(7, o0);
                        self.emit_insn(&Insn::noarg(
                            crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                        ));
                        for r in [7u8, 6, 1] {
                            self.emit_insn(&Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                                    op: crate::codegen::x86_64::opcode::StackOp::Pop,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        r,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            ));
                        }
                    } else {
                        for k in 0..n {
                            let (arg_off, off) = stack_param_stores[i + k];
                            self.load_stack(0, 16 + arg_off);
                            self.store_stack(0, off);
                        }
                    }
                    i += n;
                }
            }
            // A1: 保存 sret 指针 (rdi) 到帧底部专用槽, 供返回时写回聚合结果
            if has_sret {
                // 位于变参保存区之上、向量临时区之下 (静态链同位置, 二者互斥)
                let sret_off = self.frame_layout.sret_ptr_off;
                self.store_stack(7, sret_off);
                self.current_sret_offset = Some(sret_off);
            }
        } else {
            if let Some(body) = &f.body {
                // 过对齐帧: 预留锚点槽 (变量区最高格), 变量整体下移 8 字节.
                if self.cur_func_over_align > 0 {
                    self.var_map_total_slots = 1;
                }
                Self::pre_register_vars(
                    &mut self.var_map,
                    &body.items,
                    temp_slots,
                    cs_slots as i32,
                    kasan_rz_slots,
                    &self.static_local_map,
                    &f.name.name,
                    &mut self.var_map_total_slots,
                    &mut spill_slots,
                    self.cur_func_over_align,
                );
            }
            // `main`/无参分支无栈传参, 过对齐局部一旦存在就对齐帧基.
            if self.cur_func_over_align > 0 {
                self.cur_func_do_align = true;
                self.cur_func_anchor_off =
                    -crate::codegen::x86_64::abi::layout::LayoutMap::hdr_of(
                        temp_slots as i64,
                        cs_slots,
                    );
            }
            // 包含 padding 条目, 每个占 8 字节. 使用累加器替代 O(n) 遍历.
            self.plan_late_slot_budget(f.body.as_ref(), &f.params, &f.ret_ty);
            let var_bytes: i64 = self.var_map_total_slots.saturating_mul(8);
            // 同一份布局解算（无参/`main` 分支）。该分支今天不**参与**变参 save area /
            // 静态链 两个预留（见 `abi/layout.rs` 文件头记的「不对称 1」），于是这两条带
            // 在这里传 0：不对称从「两份公式」变成「可见的输入」。位域 RMW 带与变参分支
            // 同口径按需给：`bf_rmw_slot` 已不再保留历史回退坐标（那三个格与金丝雀、
            // callee-saved 保存区同格），两个分支必须走同一套几何。
            // 同一尺寸判据只算一次（与上面变参分支同口径）。
            let main_vec_reserve =
                crate::codegen::x86_64::abi::layout::vec_band_reserve(self.max_vec_bytes);
            let plan = crate::codegen::x86_64::abi::layout::FramePlan {
                    temp_slots: temp_slots as i64,
                    cs_slots,
                    var_bytes,
                    late_slots: self.late_slot_budget,
                    sret_need,
                    va_slots: 0,
                    static_chain_slot: 0,
                    bf_rmw_reserve: crate::codegen::x86_64::abi::layout::bf_rmw_band_reserve(self.bf_rmw_max_depth),
                    cx_band: crate::codegen::x86_64::abi::layout::cx_band_reserve(
                        self.needs_cx_band,
                    ),
                    vec_reserve: main_vec_reserve,
                    tramp_reserve: 0,
                    stack_reserve: self.stack_reserve_bytes(),
                    spill_slots,
                };
            self.frame_dump(&f.name.name, &plan);
            let layout = crate::codegen::x86_64::abi::layout::LayoutMap::resolve(&plan);
            self.frame_layout = layout;
            // sret/物化槽区顶界: 紧邻变量区底 (变量区占 -(hdr+var_bytes))。
            self.sret_region_base = layout.sret_region_base;
            // 过对齐帧: 先把 %r13/%rsp 对齐到局部最大对齐值. 必须在 sub_rsp
            // 之前, 这样 sub 出来的帧底落在对齐基线之下, 被调帧不会盖到本帧.
            self.emit_over_align_base();
            if self.frame_layout.frame_size > 0 {
                if self.target.stack_clash_protection && self.frame_layout.frame_size > 4096 {
                    self.emit_stack_clash_probes(self.frame_layout.frame_size);
                } else {
                    self.emit_sub_rsp(self.frame_layout.frame_size);
                }
            }
            // 帧已分配 ⇒ 此刻 rsp 正落在帧线上，从这里开始记「体内比帧线低多少」的账
            // （尾声据此决定发 `add $帧长,%rsp` 还是退回 `mov %r13,%rsp`）。
            self.arm_rsp_ledger();
            // -ftrivial-auto-var-init=zero: 零初始化整个栈帧. 对超大帧跳过.
            if self.target.auto_var_init_zero
                && self.frame_layout.frame_size > 0
                && self.frame_layout.frame_size <= 16 * 1024 * 1024
            {
                self.emit_zero_init_frame(self.frame_layout.frame_size);
            }
            // 过对齐帧: 把对齐前的帧顶写进锚点槽 (供尾声恢复 rsp). 须在 sub 之后,
            // 且**必须晚于零初始化**: 零初始化覆盖 `[r13-frame_size, r13)`，锚点槽
            // （`-hdr_of(temp_slots)`，实测 -0x50）正落在这一段里 —— 先写锚点再清帧
            // 会把「对齐前的帧顶」清成 0，尾声 `mov -0x50(%r13),%rsp` 于是把 rsp
            // 置 0，`pop %r13` 当场 #PF→#DF（kbuild14 boot 的停点）。
            // emit_zero_init_frame 自行 push/pop rax，对齐前的帧顶跨它存活。
            self.emit_over_align_anchor();
            // Stack canary: 在帧顶部 (r13-8) 存储金丝雀值
            if self.func_canary && self.frame_layout.canary_fits() {
                // mov rax, <guard> → 加载 canary（guard 的寻址形态见 `emit_canary_load`）
                self.emit_canary_load();
                // mov [r13 - 8], rax  → 存储在帧顶部
                self.emit_insn(&Insn::mov_rm_mem(
                    false,
                    crate::codegen::shared::asm_operand::Width::B64,
                    0,
                    13,
                    -8,
                ));
            }
            // KASAN/ASan 栈插桩：进函数先清整帧的陈旧毒，再只毒真实红区。
            // 旧写法「毒整帧 + 只解具名变量」会让晚分配带（发射期才登记）、spill 槽与
            // 对象尾部终身带毒，未插桩库正常写入就报 use-after-poison 假阳性
            // （判据本体见 `special/sanitizer.rs::emit_kasan_stack_poison_redzones`）。
            if self.kasan && self.frame_layout.frame_size > 0 {
                self.emit_kasan_stack_poison_redzones();
            }
        }
        // P13 阶段6：帧规划到此结束 —— 「哪些局部的帧槽读写每一次都是可观察事件」
        // 可以定了。先记名字→槽数（预注册与晚分配共用同一张表），再把偏移已定的那批
        // 直接登记成帧槽屏障；晚分配的会在 `register_var` 定下偏移时自己补登记。
        // 必须在任何函数体指令发射**之前**完成：块内事实一旦建立就不能事后撤销。
        if let Some(body) = &f.body {
            Self::collect_volatile_locals(
                &body.items,
                &self.static_local_map,
                &f.name.name,
                &mut self.volatile_locals,
            );
            let planned: Vec<(String, i64)> = self
                .var_map
                .iter()
                .filter(|(n, _)| self.volatile_locals.contains_key(n.as_str()))
                .map(|(n, &(off, _))| (n.clone(), off))
                .collect();
            for (n, off) in planned {
                self.note_volatile_var_slots(&n, off);
            }
            // 逃逸地址的槽同样要按**槽**登记（而不是按函数）：判据与 volatile 同源，
            // 但拦住的是「推迟存出」与「跨 call 存活」两件事。
            // 一个名字可能多次声明（遮蔽），每一个偏移都要登记。
            if !self.frame_guard.escaped_names.is_empty() {
                let hits: Vec<(i64, crate::ast::Type)> = self
                    .var_map
                    .iter()
                    .filter(|(n, _)| self.frame_guard.escaped_names.contains(n.as_str()))
                    .map(|(_, &(off, ref ty))| (off, ty.clone()))
                    .collect();
                for (off, ty) in hits {
                    let slots = Self::var_alloc_slots(&ty);
                    // 与 `note_volatile_var_slots` 同一条上限：枚举不完一个大对象时
                    // 不能慢慢插入（那是分钟级的编译时间），也不能只插一半（那就是
                    // 漏登记的逃逸槽 = 静默错值）。整体关掉本函数的家与 A1 才是保守方向。
                    if slots > crate::codegen::x86_64::instructions::VOLATILE_SLOT_CAP {
                        self.region_disable();
                        break;
                    }
                    for i in 0..slots.max(1) {
                        self.escaped_frames.insert(off.saturating_add(i * 8));
                    }
                }
            }
        }
        // VLA 形参维度求值: 序言尾 (非 main 路径两分支汇合点) 执行,
        // 此时帧已建立、参数槽与静态链槽均已写入。副作用 (++N 等) 每次
        // 调用生效 (C11 6.9.1p10); 行大小 = 维度表达式值 * 元素大小,
        // 存入形参指针槽前 off-8 的 stride 槽, 供 sizeof(a[i]) 与索引
        // 步长读取声明时绑定的行大小 (C11 6.2.4p7)。
        // 一维 VLA 形参维度副作用求值 (声明顺序, 先于 stride 求值:
        // 多维参数的内层维度表达式可能依赖外层维度副作用后的值)
        for se in &param_dim_effects {
            self.gen_expr(se, Self::EVAL_BASE);
        }
        for (pname, se, elem_sz) in &vla_param_dims {
            let off = self.var_offset(pname);
            self.gen_expr(se, Self::EVAL_BASE);
            if *elem_sz != 1 {
                self.imul_imm(0, *elem_sz as i128);
            }
            self.emit_store_rax_r13(off - 8);
        }
        // 预收集嵌套函数（编译体之前需要它们的声明列表拿 trampoline 槽）。
        // 唯一符号名已由 `compile_module` 的预扫描一次性定完（见
        // `elf::prescan_nested_names`）：这里**只查不分配**。旧形状在本函数里递增
        // 全局计数并改写三张表，并行下每个 worker 从同一份快照起号、回传又不存
        // 这些表 ⇒ 并行与串行符号名不同且嵌套函数符号整个失踪（N1）。
        let mut nested_decls: Vec<FuncDecl> = Vec::new();
        if let Some(body) = &f.body {
            Self::collect_direct_nested_funcs(&body.items, &mut nested_decls);
        }
        // 嵌套函数 trampoline 预分配 + 运行时写入 (在重命名之后):
        // 为全部直接嵌套函数预分配 trampoline 槽 (不仅是被取地址者):
        // 兄弟/后代嵌套函数的引用经静态链访问定义父帧中的 trampoline,
        // 编译后代时父帧序言已发射完毕, 无法回补槽位, 必须在此统一定位。
        let parent_name = f.name.name.clone();
        for nf_decl in &nested_decls {
            let orig_name = nf_decl.name.name.clone();
            let unique_name = self
                .nested_func_renames
                .get(&parent_name)
                .and_then(|m| m.get(&orig_name))
                .cloned()
                .unwrap_or_else(|| orig_name.clone());
            let tramp_off = self.gen_trampoline(&unique_name);
            self.nested_tramp_offsets.insert(unique_name, tramp_off);
        }
        if !self.trampoline_slots.is_empty() {
            self.emit_mprotect_for_stack();
            self.emit_trampoline_to_stack();
        }
        // -finstrument-functions: 在函数序言后插入 __cyg_profile_func_enter 调用
        if self.target.instrument_functions && !self.current_no_instrument {
            self.emit_instrument_func_enter();
        }
        let ends_ret = if let Some(body) = &f.body {
            for item in &body.items {
                // 顶层声明的行号必须在这里记：本循环把 `BlockItem::Decl(Decl::Var)` 就地
                // 展开（不经过 `gen_block_item`），而 `Stmt` 分支走 `gen_stmt` 自带行号。
                // 缺了这一条，`int t = sink(0);` 这样的声明行在 `.debug_line` 里根本没有
                // entry，gdb 的 `break file.c:那一行` 依旧绑不上。
                if let BlockItem::Decl(Decl::Var(v)) = item {
                    self.note_src_line(v.span.line);
                }
                // Stmt/Decl 边界 save/restore 物化槽水位; Decl 变量本身已加入 var_map,
                // 全程保留; 但初始化表达式内部经 alloc_sret_slot / alloc_local
                // 分配的 materialize 临时槽须在 item 结束后回收, 否则长 cld/ild
                // 链会一路累加导致 sret_watermark 与静态扫描口径失配。
                match item {
                    BlockItem::Stmt(s) => {
                        let mark = self.save_sret_watermark();
                        self.gen_stmt(s);
                        self.restore_sret_watermark(mark);
                    },
                    BlockItem::Decl(Decl::Var(v)) => {
                        let mark = self.save_sret_watermark();
                        // 函数顶层的 static 同样要在**声明点**绑定: 下面两条
                        // `continue` 之后就没有发射点了, 再往后引用只能按名字猜。
                        self.bind_static_local(v);
                        // extern 声明不分配栈槽 (与 gen_block_items / collect_frame_var_decls 口径一致)
                        if v.is_extern && v.init.is_none() {
                            continue;
                        }
                        if v.is_static
                            && self
                                .static_local_map
                                .is_static_local(&self.current_func_name, &v.name.name)
                        {
                            if self
                                .static_local_map
                                .needs_runtime_init(&self.current_func_name, &v.name.name)
                            {
                                // static local 需要运行时初始化: 发射初始化代码到全局
                                // 地址。形状判据与发射都在 `gen_static_local_init`
                                // 一处 (与 `gen_block_item` 同一个入口)。
                                self.gen_static_local_init(v);
                                continue;
                            }
                            continue; // static 局部变量已在全局数据段分配
                        }
                        // 声明处的绑定走唯一入口 `bind_decl_home`（以前这里是 `compile_func`
                        // 自己抄的一份 `gen_block_items` 逻辑，两处必须同步改）。
                        self.bind_decl_home(v);
                        if let Some(ref fn_name) = v.cleanup_fn {
                            self.cleanup_vars
                                .push((v.name.name.clone(), fn_name.clone()));
                        }
                        // 定义+声明一体的 struct/union 类型绑定 (同 gen_block_items)
                        if matches!(v.ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
                            self.gen_vm_type_bind(&v.ty);
                        }
                        if !self.decl_skips_frame_slot(v) {
                            self.register_var(&v.name.name, &v.ty);
                        }
                        if Self::type_has_vla(&v.ty) {
                            self.gen_vla_alloc(v);
                        } else if let Some(init) = &v.init {
                            if let Expr::InitList { inits, .. } = init {
                                self.gen_init_list(&v.name.name, &v.ty, inits);
                            } else if let Expr::String { data: str_data, .. } = init {
                                // 字符数组用字符串字面量初始化: char s[N] = "str";
                                // 注意: 指针初始化 (char *p = "str") 不走此路径。
                                // 生成入口与块内声明共用，两处口径不一致就是把指针字节
                                // 当数组内容的起点 (见 `gen_char_array_str_init`)。
                                if matches!(&v.ty, Type::Array(_, _)) {
                                    let (name, ty) = (v.name.name.clone(), v.ty.clone());
                                    self.gen_char_array_str_init(&name, &ty, str_data);
                                } else {
                                    // 指针初始化: 生成字符串地址
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            } else {
                                let is_vec_init = matches!(&v.ty, Type::Vector(_, _));
                                // 聚合初始化一律问 `assign_needs_whole_copy`（类型轴 ||
                                // 交付轴，报告 §5.15 的单一真源）。旧写法在这里另抄了一份
                                // 「尺寸 > 8 或 CompoundLit」的判据，漏掉了**内联展开后的
                                // StmtExpr**：`__always_inline` 构造器返回的 8 字节聚合在
                                // rax 里交付的是物化槽地址，标量 `mov_to_stack` 会把地址
                                // 写进变量（内核 `guard(mutex)` 的 `class_mutex_t`，§5.24）。
                                if is_vec_init || Self::assign_needs_whole_copy(&v.ty, init) {
                                    // struct/array/vector 初始化: 整体拷贝
                                    let dst_ty = v.ty.clone();
                                    let dst_name = v.name.name.clone();
                                    self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                                } else if let Type::Complex(inner) = &v.ty {
                                    self.gen_complex_init(&v.name.name, &inner.base, init);
                                } else if let Type::Imaginary(ref inner) = &v.ty {
                                    // _Imaginary 变量初始化: 存储单个浮点值 (虚部)
                                    self.gen_imaginary_init(&v.name.name, &inner.base, init);
                                } else if matches!(
                                    &v.ty,
                                    Type::Float | Type::Double | Type::LongDouble | Type::Float128
                                ) {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    if matches!(&v.ty, Type::Float128) {
                                        // binary128: 归一到 xmm0 的 16 字节通道
                                        self.emit_to_float128(&init.ty());
                                    } else {
                                        // int 常量初始化浮点变量: 先转换到 xmm0 (double x = 1;)
                                        self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                                    }
                                    self.gen_float_store(&v.name.name, &v.ty);
                                } else if matches!(&v.ty, Type::Int128 | Type::UInt128) {
                                    // P1-4 fix: __int128 变量初始化 (rax=low, rdx=high)
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    // 如果初始化器不是 int128 类型，需要符号扩展
                                    if !matches!(init.ty(), Type::Int128 | Type::UInt128) {
                                        if matches!(
                                            init.ty(),
                                            Type::Bool
                                                | Type::UChar
                                                | Type::UShort
                                                | Type::UInt
                                                | Type::ULong
                                                | Type::ULongLong
                                        ) {
                                            // P11: 收敛到 AluRm{Xor, B64, rdx, rdx}.
                                            let insn = Insn::new(
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
                                            );
                                            if !self.emit_insn(&insn) {
                                                self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::AluRm { op: crate::codegen::x86_64::opcode::AluOp::Xor, width: crate::codegen::shared::asm_operand::Width::B64, dst: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64), src: crate::codegen::x86_64::opcode::Operand::Reg(2, crate::codegen::shared::asm_operand::Width::B64)}, &[]));
                                            }
                                        } else {
                                            self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            )); // cqo
                                        }
                                    }
                                    let off = self.var_offset(&v.name.name);
                                    self.store_stack(0, off); // low → [r13+off]
                                    self.store_stack(2, off + 8); // high → [r13+off+8]
                                } else {
                                    self.gen_expr(init, Self::EVAL_BASE);
                                    // Implicit float→int conversion
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
                        }
                        self.restore_sret_watermark(mark);
                    },
                    // 函数体顶层的局部 struct/union 定义: 含 VLA 字段时
                    // 声明点绑定总大小槽 (与 gen_block_items 同处理)
                    BlockItem::Decl(Decl::StructDef(sd)) => {
                        self.gen_vm_struct_bind(sd, false);
                    },
                    BlockItem::Decl(Decl::UnionDef(sd)) => {
                        self.gen_vm_struct_bind(sd, true);
                    },
                    _ => {},
                }
            }
            Self::ends_with_return(&body.items)
        } else {
            false
        };
        if !ends_ret {
            // C99 5.1.2.2.3: main 无 return 语句时隐式 return 0.
            // 否则 epilogue 直接 ret, eax 为启动代码残留值.
            if f.name.name == "main" && matches!(f.ret_ty, Type::Int) {
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                        rip_relative: false,
                        width: crate::codegen::shared::asm_operand::Width::B32,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B32,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Imm(0),
                    },
                    &[],
                )); // mov eax, 0
            }
            // -finstrument-functions: 自然结尾的函数也需要 exit hook
            // (之前只在 Stmt::Return 路径发射, fallthrough 到函数末尾时遗漏)
            if self.target.instrument_functions && !self.current_no_instrument {
                self.emit_instrument_func_exit();
            }
            self.emit_func_epilogue();
        }
        // 冷格外置（`DCC_COLD_OUTLINE`）：必须在尾声之后、`func_end` 之前发。尾声以 `ret`
        // 收尾，控制流不会顺落进冷区；而 `func_end` 之后的字节会被 `func_sizes` 切片裁掉。
        // 默认旋钮下 `cold_arms` 为空，这一句不发任何字节。
        self.flush_cold_arms();
        // 区域家必须在函数结束前全部写回：字节要排在尾声之前，而 `cold_arms` 已发完。
        // 忘一步就是「变量在寄存器里死了」——函数返回后那个号被调用方拿走，帧里留旧值。
        self.region_flush();
        // 逐函数机制读数（只在旋钮开着时打，产物不受影响）。
        self.region_dump_stats(&f.name.name);
        // 合并读数（只在旋钮开着时打）：S6 的判据要能回答「到底合了几对」。
        if crate::codegen::x86_64::emit::stmt::cold_merge_on() && self.cold_merged != 0 {
            eprintln!("coldmerge fn={} merged={}", f.name.name, self.cold_merged);
        }
        // 记录函数大小
        let func_end = self.pos();
        self.func_sizes
            .insert(f.name.name.clone(), func_end - func_start);
        // Gate 0a 读数（方案 §2 0a / §8.2 修订后的判据）：`total` 是影子表本可转发而真实表
        // 没转发的总次数（= S2 的总上限），四个 `hit_*` 按「存槽之后发生得最晚的那一种
        // 屏障」归桶。判据读的是 `hit_call+hit_label+hit_pressure+hit_capacity`，不是 total。
        if crate::codegen::x86_64::blockopt::shadow::enabled() {
            let s = crate::codegen::x86_64::blockopt::shadow::dump();
            eprintln!(
                "slotshadow fn={} total={} hit_call={} hit_label={} hit_pressure={} hit_capacity={} hit_raw={} never={}",
                f.name.name,
                s.total,
                s.hit_call,
                s.hit_label,
                s.hit_pressure,
                s.hit_capacity,
                s.hit_raw,
                s.miss_never
            );
            crate::codegen::x86_64::blockopt::shadow::zero();
        }
        if crate::codegen::x86_64::blockopt::stat::enabled() {
            for (site, hits, slots_lost, copies_lost) in
                crate::codegen::x86_64::blockopt::stat::dump()
            {
                eprintln!(
                    "killstat fn={} site={} hits={} slots_lost={} copies_lost={}",
                    f.name.name, site, hits, slots_lost, copies_lost
                );
            }
            let mut vs: Vec<(String, u64, u64, u64)> =
                crate::codegen::x86_64::blockopt::stat::dump_variants();
            vs.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
            for (variant, hits, slots_lost, copies_lost) in vs {
                eprintln!(
                    "killvar fn={} variant={} hits={} slots_lost={} copies_lost={}",
                    f.name.name, variant, hits, slots_lost, copies_lost
                );
            }
            crate::codegen::x86_64::blockopt::stat::zero();
            crate::codegen::x86_64::blockopt::stat::zero_variants();
        }
        // C++ 零成本异常: 归档本函数的受保护调用点与 CFI 事实。必须在嵌套函数
        // 生成之前完成 (嵌套函数会重新 clear `eh_sites`)。
        if !self.eh_sites.is_empty() {
            self.eh_funcs
                .push(crate::codegen::x86_64::eh_tables::EhFuncInfo {
                    name: f.name.name.clone(),
                    text_start: func_start,
                    sites: std::mem::take(&mut self.eh_sites),
                });
        }
        self.func_cfi.insert(
            f.name.name.clone(),
            crate::codegen::x86_64::eh_tables::FuncCfi {
                prologue_end: self.cur_prologue_end,
                callee_saved_used: self.reg_bank.used_mask(),
                frame_pointer: self.target.frame_pointer,
                // 基址常驻在序言那一刻才最终定下（帧顶放得下吗），而这一行在函数体之后，
                // 读到的就是最终值：序言里清成 None 的那些函数不会多带这条规则。
                rbp_saved: self.disptab_base_reg_sym.is_some(),
            },
        );
        // 函数体+尾声已生成, callee_saved_used 完整: 回填序言/尾声中
        // 未使用寄存器的保存/恢复指令为 nop. 必须在嵌套函数生成前
        // (嵌套函数会 reset tracker 并追加自己的 sites).
        self.patch_callee_saves(callee_sites_base);
        // Phase 6.2：本帧的位置表在此刻是完整的（`homes` 与 `var_map` 都是最终值），
        // 而下一个函数一开始就把两者复位 —— 所以采集必须卡在这里，嵌套函数之前
        // （见 `collect_debug_var_facts` 的注释）。
        self.collect_debug_var_facts(f, func_start, self.pos());
        // 父函数布局确定后再生成嵌套函数，把捕获变量的真实 r13 偏移传入。
        // 使用预收集的 nested_decls 和预注册的重命名
        if !nested_decls.is_empty() {
            let parent_var_map = self.var_map.clone();
            let parent_name = f.name.name.clone();
            let parent_static_chain_off = self.static_chain_offset;
            // 父帧长必须在循环**外**取一次快照：`compile_func` 会把
            // `self.frame_layout` 换成子函数自己的且返回时不恢复（父的体/尾声
            // 此时已发完，本就不该再读它）。循环里每轮重读就会让第二个及以后
            // 的嵌套函数拿到**上一个兄弟**的帧长 —— 实测 torture `pr128464.c`：
            // g1 的 nonlocal goto 用 `lea -112(%r13),%rsp`（f 帧 112 ✓），到 g2
            // 变成 `lea -48(%r13),%rsp`（g1 的帧 48 ✗），跳回 f 的 .Ll_f_l2 后
            // `add $112,%rsp; pop %r13; ret` 从帧外取返回地址，SIGSEGV。
            let parent_frame_size = self.frame_layout.frame_size;
            // 父级 upvars: name → (origin_offset, chain_depth, intermediate_offsets)
            let parent_upvar_info: HashMap<String, (i64, u32, Vec<i64>)> = f
                .upvars
                .iter()
                .map(|(n, o)| {
                    let off = *o;
                    // 父函数的 upvar 深度 = 当前 upvar_chain_depth.get(n)?
                    // 若无 (一级 upvar), 默认为 1, intermediate 为空
                    let depth = self.upvar_chain_depth.get(n).copied().unwrap_or(1);
                    let intermediates = self
                        .upvar_intermediate_offsets
                        .get(n)
                        .cloned()
                        .unwrap_or_default();
                    (n.clone(), (off, depth, intermediates))
                })
                .collect();
            for mut nf_decl in nested_decls {
                // 计算 upvar 偏移 + 链深度 + 中间偏移
                // intermediates 语义: load_upvar 先做一次
                //   r11 = [child_r13 + child.chain_off] → 父帧
                // 然后对每个 intermediate 做
                //   r11 = [r11 + intermediate] → 再上溯一层
                // 最后访问 [r11 + origin_off]
                // depth=1 (直接父): intermediates = []  (父帧即起源帧)
                // depth=2 (祖父): intermediates = [parent.chain_off]  (再上溯一次到祖父)
                let mut new_upvars: Vec<(String, i64, u32, Vec<i64>)> = Vec::new();
                for (name, off) in &mut nf_decl.upvars {
                    if let Some((parent_off, _)) = parent_var_map.get(name) {
                        // 直接父级 upvar: depth=1, intermediates=[]
                        *off = *parent_off;
                        new_upvars.push((name.clone(), *off, 1, vec![]));
                    } else if let Some(&parent_off) = self.vla_offsets.get(name) {
                        // VLA upvar: 元数据槽在 vla_offsets 里 (不在 var_map),
                        // 父帧 [parent_off] 是元数据指针槽, 二次加载才是数据基址
                        // (921017-1.c: 嵌套函数 g 读外层 VLA a)。
                        *off = parent_off;
                        new_upvars.push((name.clone(), *off, 1, vec![]));
                    } else if let Some((origin_off, parent_depth, parent_intermediates)) =
                        parent_upvar_info.get(name)
                    {
                        // 祖父级 upvar: depth+1, intermediates = [parent.chain_off] + parent_intermediates
                        let mut intermediates = vec![parent_static_chain_off];
                        intermediates.extend(parent_intermediates.iter().copied());
                        *off = *origin_off;
                        new_upvars.push((name.clone(), *off, parent_depth + 1, intermediates));
                    } else {
                        eprintln!(
                            "[WARN] nested upvar {} not found in parent var_map or upvars",
                            name
                        );
                    }
                }
                // 写入新的 upvars + 临时设置 self 的链深度/中间偏移
                nf_decl.upvars = new_upvars
                    .iter()
                    .map(|(n, o, _, _)| (n.clone(), *o))
                    .collect();
                // 保存父级链状态, 设置子级链状态
                let saved_upvar_chain_depth = self.upvar_chain_depth.clone();
                let saved_upvar_intermediate = self.upvar_intermediate_offsets.clone();
                self.upvar_chain_depth.clear();
                self.upvar_intermediate_offsets.clear();
                for (name, _off, depth, intermediates) in &new_upvars {
                    self.upvar_chain_depth.insert(name.clone(), *depth);
                    self.upvar_intermediate_offsets
                        .insert(name.clone(), intermediates.clone());
                }
                // 从预注册的重命名映射中查找唯一名称
                let orig_name = nf_decl.name.name.clone();
                let unique_name = self
                    .nested_func_renames
                    .get(&parent_name)
                    .and_then(|m| m.get(&orig_name))
                    .cloned()
                    .unwrap_or(orig_name.clone());
                // 重命名函数声明并编译
                nf_decl.name.name = unique_name.clone();
                // 将嵌套函数加入 defined_funcs，这样 ELF 发射器知道它的符号
                self.defined_funcs.insert(unique_name.clone());
                // 非局部 goto 要用父帧长把 %rsp 拉回父帧底（见
                // `Codegen::nonlocal_parent_frame`）。用循环外取的
                // `parent_frame_size` 快照：父函数正在发体中段时
                // `frame_layout` 已解算定稿，但编完一个子函数就会被子函数覆盖。
                let saved_parent_frame = self.nonlocal_parent_frame;
                self.nonlocal_parent_frame = Some(parent_frame_size);
                self.compile_func(&nf_decl, prog);
                self.nonlocal_parent_frame = saved_parent_frame;
                // 恢复父级链状态 (父函数可能还有兄弟嵌套函数)
                self.upvar_chain_depth = saved_upvar_chain_depth;
                self.upvar_intermediate_offsets = saved_upvar_intermediate;
            }
        }
        // 缺陷 B: 本函数 (含上面刚发的嵌套函数) 收尾时, 指向内部 `.L` 标号的
        // 跳转必须已全部可解析。并行 worker 里这一条把「标号在别的 chunk 被
        // 同名覆盖」从静默误编译变成发射期错误 (而不是等到合并/回填之后)。
        self.check_pending_labels_closed(&f.name.name);
        // P1 对账：生成期 sret_watermark 峰值不得超过规划期 sret_need 预算。
        // 漏算的站点 (F9 那类复数变宽 cast 链) 会在这里以 ICE 现形，而不是
        // 在运行期压穿帧底 (C1 的"静默错值" → 编译期归因)。
        self.reconcile_sret_watermark();
    }

    /// P1: `reconcile_frame_demand` 的第一个子对账——sret/物化带。
    /// 对比生成期累积的实际水位 `sret_watermark` 与规划期静态扫描得出的
    /// `frame_layout.sret_need`。溢出即 ICE (E1128)，同时给出行级提示指
    /// 到 `func_setup.rs::sret_bytes_expr` 需要登记的站点。
    fn reconcile_sret_watermark(&mut self) {
        if self.frame_layout.frame_size <= 0 {
            // unresolved() 占位帧 (规划尚未走)：裸函数 / naked / 内联 asm。
            return;
        }
        let peak = self.sret_watermark;
        let budget = self.frame_layout.sret_need;
        if peak <= budget {
            return;
        }
        self.report_internal_bug(
            "E1128",
            format!(
                "sret watermark exceeded plan in `{}`: generated {}B peak vs planned {}B budget \
                 (overshoot {}B). A new `alloc_sret_slot` call site was not reflected in \
                 `max_sret_bytes` in func_setup.rs.",
                self.current_func_name,
                peak,
                budget,
                peak - budget,
            ),
            "Look for an unregistered alloc_sret_slot site and add its bytes to \
             sret_bytes_expr / SretRule.  Silence until P3 rolls out honest scanning.",
        );
    }
}
