// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::ast::{
    AddrSpace, ArraySize, BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, InitItem, Program,
    QualifierFlags, Stmt, VarDecl,
};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;

// ---------------------------------------------------------------------------
// Inline policy constants
//
// These govern the inliner's behaviour at each optimisation level.
// The Linux kernel relies on `__always_inline` / `__attribute__((always_inline))`
// being honoured unconditionally, and on `__attribute__((noinline))` preventing
// inlining.  Getting these wrong can cause functional correctness issues
// (e.g. percpu access macros, bit-op inline functions).
// ---------------------------------------------------------------------------

/// Maximum call-chain depth for speculative (non-forced) inlining.
/// `always_inline` / `flatten` bypass this limit.
const INLINE_MAX_DEPTH: u32 = 8;

// ---------------------------------------------------------------------------
// Inline decision tracing
//
// Set DCC_INLINE_TRACE=1 in the environment to emit one line per inline
// decision to stderr.  Useful for debugging kernel builds where
// __always_inline / noinline correctness is critical.
// ---------------------------------------------------------------------------

static INLINE_TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

fn inline_trace_enabled() -> bool {
    *INLINE_TRACE_ENABLED.get_or_init(|| {
        std::env::var("DCC_INLINE_TRACE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Emit a single trace line for an inline decision.
/// Format: `dcc-inline: <decision> <callee> [reason]`
fn trace_inline_decision(callee: &str, decision: &str, reason: &str) {
    if inline_trace_enabled() {
        eprintln!("dcc-inline: {} {} ({})", decision, callee, reason,);
    }
}

/// 按 -O 级别运行 IR 优化。
///
/// 优化语义约束:
/// - `-fno-strict-aliasing` (默认): 不做类型双关假设 (TBAA). 当前别名分析
///   (P4 Steensgaard) 是类型不敏感的, 不区分不同指针类型的别名关系, 已
///   保守处理. 未来若添加 TBAA 需检查 target.strict_aliasing.
/// - `-fno-strict-overflow` (默认): 不假设有符号溢出为未定义行为. 常量折
///   叠使用 wrapping 算术, 循环优化不做基于无溢出的范围推断. 未来若添
///   加基于有符号溢出 UB 的优化需检查 target.strict_overflow.
/// - 常量折叠 (fold_constants): 编译期常量使用 wrapping_add/sub/mul,
///   与 C 标准整数语义一致 (模算术).
///
/// 内联策略:
/// - `-O0`: 仅内联 `always_inline` / `flatten` 标记的函数 (内核正确性
///   依赖, 如 percpu 宏). 不受尺寸限制.
/// - `-O1-O2`: 仅 `always_inline` / `flatten` (未来计划加入成本模型
///   驱动的 `static inline` 内联).
/// - `-O3`: 额外启发式内联小型 `static` 零参函数 (cost ≤ 20 AST nodes).
/// - `noinline` 属性始终被尊重 (除非与 `always_inline` 冲突, 此时
///   `always_inline` 优先, 与 GCC 行为一致).
/// - speculative 内联深度上限 `INLINE_MAX_DEPTH` (8). 递归函数
///   通过 callee-name 检测阻止直接递归内联.
pub fn optimize(module: &mut Module, level: u8) {
    optimize_with_target(module, level, false, 0);
}

/// 带目标信息的优化入口。
///
/// `has_vnni`: 目标是否支持 VNNI 指令
/// `vec_bits`: 向量寄存器位宽 (256/512)
pub fn optimize_with_target(module: &mut Module, level: u8, has_vnni: bool, vec_bits: u32) {
    // Default: strict_aliasing=true (C standard semantics), debug_info=false
    // (`optimize_with_target` 的既有调用方都不带 -g 信息; 需要时走
    // `optimize_with_full_target`)。
    optimize_with_full_target(module, level, has_vnni, vec_bits, true, false);
}

/// 带完整目标信息的优化入口。
///
/// `strict_aliasing`: -fno-strict-aliasing 时为 false, 别名分析合并所有指针
pub fn optimize_with_full_target(
    module: &mut Module,
    level: u8,
    has_vnni: bool,
    vec_bits: u32,
    strict_aliasing: bool,
    debug_info: bool,
) {
    // 内联: O0-O2 仅 always_inline/flatten; O3 额外启发式
    if !opt_skipped("function_inline") {
        function_inline(module, level);
    }
    if level == 0 {
        return;
    }
    // IPA modref: 跨过程 "哪些指针形参被写穿" 摘要 + 只对「地址逃逸点全部是已证明
    // 不写的调用实参」的局部折叠 __builtin_constant_p。必须排在 fold_constants 之前:
    // 折出的 Int(1) 之后的 fold_expr / SCCP 才能把 `!1` 折成 0、删掉 `if(!1) abort();`。
    if level >= 2 && !opt_skipped("ipa_modref") {
        crate::ipa_modref::run(module, level);
    }
    if !opt_skipped("fold_constants") {
        fold_constants(module);
    }
    if level >= 2 {
        if !opt_skipped("prune_dead_stores") {
            prune_dead_stores(module);
        }
        // Dead asm elimination: the SSA DCE pass removes pure
        // (non-volatile, no outputs, no memory clobber) asm when
        // its results are unused, matching GCC behavior. Volatile
        // asm and asm with side effects are always preserved.
        if !opt_skipped("fold_constant_branches") {
            fold_constant_branches(module);
        }
        if !opt_skipped("eliminate_unreachable") {
            eliminate_unreachable(module);
        }
        if !opt_skipped("ast_gvn") {
            crate::ast_gvn::run(module, level);
        }
        if !opt_skipped("loop_opt") {
            crate::loop_opt::run_with_target(module, level, has_vnni, vec_bits);
        }
        // M16: 跨函数 DCE (call graph reachability)
        if !opt_skipped("cross_function_dce") {
            cross_function_dce(module);
        }
        // Qualifier-aware optimization: const/volatile/restrict
        if !opt_skipped("qualifier_aware") {
            qualifier_aware_optimization(module);
        }
    }
    if level >= 2 {
        // M16: SSA pass pipeline (strength reduction, IVOP, TCO, etc.)
        if !opt_skipped("run_ssa_passes") {
            run_ssa_passes(module, level, strict_aliasing, debug_info);
        }
    }
}

/// 诊断用：`DCC_OPT_SKIP=prune_dead_stores,ast_gvn,loop_opt,...` 逐个关掉 AST 层 pass。
///
/// 为什么必须有它：优化器改动行为时的定位手段只有「关掉某个 pass 再看产物」，
/// 而没有开关就只能读代码猜。本轮就是这么错过一次的：`-O2` 下浮点一元负号被
/// 降级成 `addsd +0.0`（错值），先前的二分只能覆盖 SSA 层（`DCC_SSA_OFF`），
/// 而那个缺陷恰好在 AST 层。两个开关合起来才是整条管线（名字与本函数里的调用一一对应）。
fn opt_skipped(name: &str) -> bool {
    std::env::var("DCC_OPT_SKIP")
        .map(|v| v.split(',').any(|s| s.trim() == name))
        .unwrap_or(false)
}

/// M16: 把 AST -> SSA, 跑 ssa::passes::run, 再 SSA -> AST 重建。
/// 含 goto / label / switch 的函数自动跳过 (走 is_structured 检测)。
/// SSA builder 通过 insert_loop_phis 为循环回边插入 phi 节点,
/// rebuild_cycle 将循环 CFG 重建为 while 循环 AST。
/// 注意: rebuild_cycle 对含循环函数的重建尚不完全正确,
/// 因此含循环的函数仍走 AST pass 路径。
fn run_ssa_passes(module: &mut Module, level: u8, strict_aliasing: bool, debug_info: bool) {
    let prog = module.source_mut();
    // Collect volatile global variable names - SSA passes must not
    // propagate constants through volatile accesses.
    let volatile_globals: std::collections::HashSet<String> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Var(v) if v.quals.is_volatile() => Some(v.name.name.clone()),
            _ => None,
        })
        .collect();
    // file-scope 的 `register asm` 变量占住的号要让开（循环里不能再借 `prog`，所以先算）。
    let taken_regs = file_scope_bound_regs(&prog.decls);
    // 诊断窗口：`DCC_MEMDBG=1` 逐函数打印 SSA 往返前后的本进程 RSS 与耗时。
    // 为什么必须有：`-O2` 编 `sqlite3.c` 的 RSS 随函数数单调上涨（实测 >4 GB，而
    // `-O0`/跳掉本 pass 只要 1.6 GB），没有逐函数读数就分不清「某个函数 IR 爆炸」和
    // 「每个函数都留下一块不回收的内存」这两种完全不同的根因。只写 stderr，不进产物。
    let memdbg = std::env::var("DCC_MEMDBG").is_ok();
    let mut memdbg_idx = 0usize;
    // 诊断窗口：`DCC_SSA_ONLY_FN=<name>` 只让同名函数进 SSA 管线，其余保持 AST 原形。
    // 用途：把「单函数的病理」（实测 `updateAccumulator` 一次 SSA 往返吃 938 MB / 22.8 s）
    // 与整文件的固定成本（AST pass + 后端）分开，一次实验从分钟级降到十秒级。
    // 与 `DCC_OPT_SKIP` 同级：只在报障时设置，不参与任何判据，不进产物字节。
    let ssa_only = std::env::var("DCC_SSA_ONLY_FN").ok();
    // 同上，但名单来自文件（一行一个函数名）：整文件级别的分诊（ddmin 二分「哪几个函数
    // 进 SSA 就复现错值」）只能靠名单，单名开关要跑几千轮。
    let ssa_only_list: Option<std::collections::HashSet<String>> =
        std::env::var("DCC_SSA_ONLY_LIST")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| {
                s.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            });
    // returns_twice 函数名全集: 已知 libc 名字 ∪ 声明上带 returns_twice 属性的函数。
    let rt_names: std::collections::HashSet<String> = {
        let mut s: std::collections::HashSet<String> =
            KNOWN_RETURNS_TWICE.iter().map(|n| (*n).to_string()).collect();
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.returns_twice {
                    s.insert(f.name.name.clone());
                }
            }
        }
        s
    };
    // returns_twice 影响域 (cone): 直接含 returns_twice 调用的函数 + 传递调用它们的函数。
    // 这条链上的函数要么自身有「二次返回」控制流汇合点, 要么身处其调用链中参与非局部
    // 控制流, 整条链退出 SSA 管线走保守的 AST 路径最稳妥 (见下方退避)。
    let rt_cone = compute_rt_cone(prog, &rt_names);
    let mut modified = false;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            // 含循环的函数**从本轮起进入** SSA 管线。旧实现在此 `continue`, 理由是
            // 「rebuild_cycle 对循环 CFG 的重建尚不完全正确」; 逐层查下去, 真正缺的是
            // 四个上游不变式, 本轮已逐项修完并有测试兜底:
            //   * `insert_loop_phis` 只从 header 收集使用且不改写体内引用 ⇒ 循环携带值
            //     在 IR 里根本没经过 phi (SCCP 因此能把 `s=0` 直传进循环体);
            //   * `bb.preds`/`succs` 缓存改过 terminator 后不刷新 ⇒ `find_loop_body_multi`
            //     沿旧边循环体算错;
            //   * 构建器把 `for.cond` 的入口 Branch 改写成 `Jump` ⇒ 零次迭代的 `for`
            //     多跑一圈 (`nest(0)` 由 0 变 1);
            //   * 调度/值编号/投机上提只看 SSA 依赖, 而重建按**基名塌缩**成可变量 ⇒
            //     同基名的读写序就是语义。
            // 证据: `tests/fixtures/opt/ssa_loop_shapes.c` 与 326 个夹具的 -O0≡-O2≡-O3
            // 扫描在解锁下与不解锁持平, 零额外失效。
            // 非结构化控制流（goto / label / computed goto）的判据：决定 SSA 通道
            // **怎么落地**，不再决定准入（见 `ssa::structure::has_unstructured_flow`）。
            // 它在准入判据之前算：`func_has_unsafe_ssa_pattern` 里「static const 局部」
            // 那一条只对**会重建**的通道成立（重建会丢掉 `is_static`），analysis-only
            // 不重建、不产代码，所以那里要放行（见该函数内的注释）。
            let unstructured = f
                .body
                .as_ref()
                .is_some_and(|b| crate::ssa::structure::has_unstructured_flow(b));
            // Skip functions with unsafe SSA patterns
            if func_has_unsafe_ssa_pattern(f, unstructured) {
                continue;
            }
            // returns_twice 影响域 (含 setjmp 的函数及其调用链): 二次返回会让 SSA 在
            // 调用点两侧做的移动/寄存器提升 / 实参槽位重叠破坏栈态 —— 整函数退出 SSA
            // 往返, 退回与 -O1 同口径的 AST 路径 (经验证 -O0/-O1 完全正确)。
            if rt_cone.contains(&f.name.name) {
                if std::env::var("DCC_SSA_BAIL").is_ok() {
                    eprintln!("dcc: SSA bail: returns_twice cone ({})", f.name.name);
                }
                continue;
            }
            if let Some(only) = &ssa_only {
                if &f.name.name != only {
                    continue;
                }
            }
            if let Some(set) = &ssa_only_list {
                if !set.contains(&f.name.name) {
                    continue;
                }
            }
            // Skip functions that access volatile objects. 除了 volatile 全局,
            // **volatile 局部/参数**同样要跳过: SSA pass 会在它们上面做常量传播与
            // 值复用, 把第二次读换成前一次的求值结果 (volatile 要求每次读都真的访存),
            // 且 int128 一类的宽值在 SSA 往返后会被拼错 (`s128 = u128` 这条
            // 128 位搬运实测在 -O2 变成 high+high, 结果整个量级错掉)。
            if func_refs_volatile(f, &volatile_globals) {
                continue;
            }
            // P0-4: 先将 switch 降级为 if-chain, 使含 switch 的函数也能 SSA 化
            crate::ssa::lower_switches_in_func(f);
            let memdbg_t0 = memdbg.then(std::time::Instant::now);
            let mut memdbg_ir = (0usize, 0usize);
            let mut memdbg_marks: Vec<(&'static str, u64)> = Vec::new();
            let built = if unstructured {
                // 非结构化通道：不因「急切求值」退避（那条危害只存在于重建后的产物上）。
                crate::ssa::build_from_func_analysis(f)
            } else {
                crate::ssa::build_from_func(f)
            };
            if let Some(mut ssa) = built {
                if memdbg {
                    memdbg_ir = (
                        ssa.blocks.len(),
                        ssa.blocks
                            .values()
                            .map(|b| b.insts.len() + b.phis.len())
                            .sum(),
                    );
                    memdbg_marks.push(("built", memdbg_rss_kb()));
                }
                // TU 级 FENV_ACCESS 事实随 IR 下去: GVN 等值编号 pass 靠它判定
                // 「浮点运算不得复用」(舍入模式可在运行期被 fesetround 改掉)。
                ssa.fenv_access = prog.fenv_access;
                // analysis-only 通道：含 goto/label/computed goto 的函数**不重建**。
                //
                // 它要的只是两件事实：哪些局部其实一直活在一个号上（活性/干涉），
                // 以及它们的家该是哪个号。这两件事在 IR 上算得准，而「把不可约 CFG
                // 重新结构化成一棵 while/if 树」是重建器做不到的（`rebuild_cycle` 按
                // 循环头展开路径，要求 CFG 可约）。旧口径是整函数连准入都拒掉，于是
                // 解释器主循环里的 `base/pc/cl/k` 连候选都不是。
                //
                // 分配结果直接写回**原封不动的 AST** 声明（`VarDecl::home`），所以这条
                // 路径上没有 phi 物化、没有表达式内联、没有块序重排 —— 产物只在
                // 「某个局部从栈槽变成了 callee-saved 号」这一件事上变化。
                if unstructured {
                    if !opt_skipped("ssa_regalloc") {
                        let ctx = crate::ssa::regalloc::Ctx {
                            debug_info,
                            taken_regs: taken_regs.clone(),
                            unstructured: true,
                        };
                        let plan = f
                            .body
                            .as_ref()
                            .map(|orig| crate::ssa::regalloc::plan_and_split(&mut ssa, orig, &ctx));
                        if let (Some(mut plan), Some(body)) = (plan, f.body.as_mut()) {
                            crate::ssa::regalloc::deliver(&ssa, &mut plan, body);
                            modified = true;
                        }
                    }
                    if let Some(t0) = memdbg_t0 {
                        memdbg_idx += 1;
                        eprintln!(
                            "dcc: memdbg #{memdbg_idx} {} ir={}b/{}i analysis-only dt={}ms",
                            f.name.name,
                            ssa.blocks.len(),
                            ssa.blocks
                                .values()
                                .map(|b| b.insts.len() + b.phis.len())
                                .sum::<usize>(),
                            t0.elapsed().as_millis()
                        );
                    }
                    continue;
                }
                crate::ssa::passes::run_with_opts(&mut ssa, level, strict_aliasing);
                if memdbg {
                    memdbg_marks.push(("post-passes", memdbg_rss_kb()));
                }
                // 诊断窗口：`DCC_SSA_IR_DUMP=1` 打印过完 SSA 管线的 IR 与重建后的 AST。
                // 为什么必须有：这两样之间发生的事（内联、类型还原、基名塌缩）在产物上
                // 常常只剩一个错值或一个丢掉的限定符，没窗口就只能猜。只在报障时打开，
                // 不进任何判据。
                if std::env::var("DCC_SSA_IR_DUMP").is_ok() {
                    let want = std::env::var("DCC_SSA_IR_DUMP_FN").ok();
                    if want.as_deref().map(|w| f.name.name == w).unwrap_or(true) {
                        eprintln!(
                            "==== [SSA IR] {} ====\n{}",
                            f.name.name,
                            crate::ssa::dump_function(&ssa)
                        );
                    }
                }
                // 注：这里曾有一个 `stabilize_addr_def_use` 事后把被落到后面的地址生产者
                // 往前搬的补丁。它的根因（`ssa::passes::collect_uses` 对 `IndexAddr`/`MemberAddr`/`Invoke`
                // 盲视，导致调度/投机在块内造出 use-before-def）已在那一份依赖判据里穷举修掉，
                // 所以上面那个搬动不再需要（实测：去掉它之后 lvalue_addr_rows 夹具仍逐档与 gcc 一致）。
                // 循环的续体与外层构造共享时，结构化重建表达不了（判据与实测两端失败见
                // `has_multi_exit_cycle`）⇒ 整函数退出 SSA 往返，f 保持 AST 原形。
                if crate::ssa::has_multi_exit_cycle(&ssa) {
                    if std::env::var("DCC_SSA_BAIL").is_ok() {
                        eprintln!("dcc: SSA bail: 共享续体的循环 ({})", f.name.name);
                    }
                    continue;
                }
                // P12 Phase 5.0：**决策在重建之前**。`plan_and_split` 只看 IR 与重建前的体，
                // 并把版本分裂表写进 `ssa.local_splits` —— 重建必须看得见它，才能给不同
                // 版本生成各自的局部。只有 x86_64 后端读这张表，其他后端产物不受影响。
                let regalloc_plan = if opt_skipped("ssa_regalloc") {
                    None
                } else {
                    let ctx = crate::ssa::regalloc::Ctx {
                        debug_info,
                        taken_regs: taken_regs.clone(),
                        unstructured: false,
                    };
                    f.body
                        .as_ref()
                        .map(|orig| crate::ssa::regalloc::plan_and_split(&mut ssa, orig, &ctx))
                };
                if memdbg {
                    memdbg_marks.push(("post-regalloc-plan", memdbg_rss_kb()));
                }
                // 原形是作废路径的退路：重建失败时把它原样放回，所以这里**借**而不是克隆
                // （克隆会把整棵 AST 再抄一份，正是要避免的那种开销）。
                let orig_body = f.body.take();
                let rebuilt = crate::ssa::rebuild_block(&ssa);
                if std::env::var("DCC_DUPARG_DUMP").is_ok() {
                    dbg_dup_call_args(&f.name.name, &rebuilt);
                }
                if memdbg {
                    memdbg_marks.push(("rebuilt", memdbg_rss_kb()));
                }
                if crate::ssa::rebuild_over_budget() {
                    // 路径展开超预算 ⇒ 本次重建作废，函数保持 SSA 往返之前的 AST 原形
                    // （correct-by-bail，与 `has_multi_exit_cycle` 的退路同一处置）。
                    if std::env::var("DCC_SSA_BAIL").is_ok() {
                        eprintln!(
                            "dcc: SSA bail: 重建展开超预算 ({}, steps={}, 单头 peak={})",
                            f.name.name,
                            crate::ssa::rebuild_steps(),
                            crate::ssa::rebuild_expansion_peak()
                        );
                    }
                    f.body = orig_body;
                } else if crate::ssa::rebuild_broke_lazy_guard() {
                    // `&&` / `||` 的右操作数没能整体内联 ⇒ 那个值会作为一条独立语句发在
                    // 带守卫的分支之前, 被守卫保护的那次求值因此提前发生 (读未初始化的值、
                    // 或直接打空)。SSA 通道不为短路求值造块, 重建之后没有任何地方还能把它
                    // 搬回去 ⇒ 整函数退出 SSA 往返 (correct-by-bail，判据真源见
                    // `crate::ssa::inline_temps::lazy_operand_landed`)。
                    if std::env::var("DCC_SSA_BAIL").is_ok() {
                        eprintln!("dcc: SSA bail: 短路右操作数被落到守卫之外 ({})", f.name.name);
                    }
                    f.body = orig_body;
                } else {
                    f.body = Some(rebuilt);
                    if let (Some(mut plan), Some(body)) = (regalloc_plan, f.body.as_mut()) {
                        // 交付（重建之后）：先校验后置事实（拿的就是重建后的体），再把位置表写进 AST。
                        // 决策阶段已经用完重建前的体，这里不再需要它（Phase 5.0 的顺序证据）。
                        crate::ssa::regalloc::deliver(&ssa, &mut plan, body);
                    }
                    if memdbg {
                        memdbg_marks.push(("post-deliver", memdbg_rss_kb()));
                    }
                    modified = true;
                }
            }
            // 本函数的 SSA `Function` 已随上面的 `if let` 块结束而释放，此刻的 RSS 才是
            // 「留给下一个函数的存量」——单调上涨就是留存，个别尖峰就是单函数 IR 爆炸。
            if let Some(t0) = memdbg_t0 {
                memdbg_idx += 1;
                let marks = memdbg_marks
                    .iter()
                    .map(|(l, r)| format!("{l}={r}KB"))
                    .collect::<Vec<_>>()
                    .join(" ");
                eprintln!(
                    "dcc: memdbg #{memdbg_idx} {} ir={}b/{}i steps={} exp={} {marks} kept={}KB dt={}ms",
                    f.name.name,
                    memdbg_ir.0,
                    memdbg_ir.1,
                    crate::ssa::rebuild_steps(),
                    crate::ssa::rebuild_expansion_peak(),
                    memdbg_rss_kb(),
                    t0.elapsed().as_millis()
                );
            }
        }
    }
    let _ = modified;
}

/// `DCC_MEMDBG` 的读数来源：本进程当前常驻集（KB）。
///
/// 临时诊断：扫描重建后的 AST，报告参数里出现「结构相同实参」的 Call。
/// 仅 `DCC_DUPARG_DUMP` 开启时由 SSA 往返后调用，不进产物字节。
fn dbg_dup_call_args(func_name: &str, b: &crate::ast::Block) {
    use crate::ast::{BlockItem, Expr, Stmt};
    fn walk_block<'a>(b: &'a crate::ast::Block, out: &mut Vec<&'a Expr>) {
        for bi in &b.items {
            walk_item(bi, out);
        }
    }
    fn walk_item<'a>(bi: &'a BlockItem, out: &mut Vec<&'a Expr>) {
        match bi {
            BlockItem::Stmt(s) => walk_stmt(s, out),
            BlockItem::Decl(_) => {}
        }
    }
    fn walk_stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a Expr>) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(v) = val {
                    walk_expr(v, out);
                }
            }
            Stmt::Expr { e, .. } => walk_expr(e, out),
            Stmt::If { cond, then_, else_, .. } => {
                walk_expr(cond, out);
                walk_stmt(then_, out);
                if let Some(x) = else_ {
                    walk_stmt(x, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                walk_expr(cond, out);
                walk_stmt(body, out);
            }
            Stmt::Do { body, cond, .. } => {
                walk_stmt(body, out);
                walk_expr(cond, out);
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(x) = init {
                    walk_stmt(x, out);
                }
                if let Some(x) = cond {
                    walk_expr(x, out);
                }
                if let Some(x) = step {
                    walk_expr(x, out);
                }
                walk_stmt(body, out);
            }
            Stmt::Block { b, .. } => walk_block(b, out),
            Stmt::Label { stmt, .. } => walk_stmt(stmt, out),
            Stmt::Switch { expr, cases, prelude, .. } => {
                walk_expr(expr, out);
                for p in prelude {
                    walk_item(p, out);
                }
                for c in cases {
                    if let Some(v) = &c.val {
                        walk_expr(v, out);
                    }
                    if let Some(v) = &c.range_end {
                        walk_expr(v, out);
                    }
                    walk_block(&c.body, out);
                }
            }
            Stmt::Invoke { call, .. } => walk_stmt(call, out),
            Stmt::AsmGoto { outputs, inputs, .. } => {
                for o in outputs {
                    walk_expr(o, out);
                }
                for i in inputs {
                    walk_expr(i, out);
                }
            }
            _ => {}
        }
    }
    fn walk_expr<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
        match e {
            Expr::Call { callee, args, .. } => {
                out.push(e);
                walk_expr(callee, out);
                for a in args {
                    walk_expr(a, out);
                }
            }
            Expr::Binop { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            }
            Expr::Unary { e: x, .. } => walk_expr(x, out),
            Expr::Cast { e: x, .. } => walk_expr(x, out),
            Expr::AddrOf { e: x, .. } => walk_expr(x, out),
            Expr::Deref { e: x, .. } => walk_expr(x, out),
            Expr::Index { arr, idx, .. } => {
                walk_expr(arr, out);
                walk_expr(idx, out);
            }
            Expr::Member { expr, .. } => walk_expr(expr, out),
            Expr::Assign { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            }
            Expr::Cond { cond, then_, else_, .. } => {
                walk_expr(cond, out);
                walk_expr(then_, out);
                walk_expr(else_, out);
            }
            Expr::Comma { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            }
            Expr::StmtExpr { block, .. } => walk_block(block, out),
            Expr::PostInc { e: x, .. }
            | Expr::PostDec { e: x, .. }
            | Expr::PreInc { e: x, .. }
            | Expr::PreDec { e: x, .. } => walk_expr(x, out),
            _ => {}
        }
    }
    let mut all = Vec::new();
    walk_block(b, &mut all);
    for e in &all {
        if let Expr::Call { callee, args, .. } = e {
            if args.len() >= 2 {
                let dbgs: Vec<String> = args.iter().map(|a| format!("{:?}", a)).collect();
                let mut dup = false;
                for i in 0..dbgs.len() {
                    for j in (i + 1)..dbgs.len() {
                        // 只在「涉及变量/地址」的实参上判定：常量重复（如两个字面量 0）
                        // 通常是源码本意，不是塌缩 bug。塌缩 bug 的标志是多个本应不同的
                        // 变量/取址实参变成同一个 Ident。
                        if dbgs[i] == dbgs[j]
                            && (dbgs[i].contains("Ident")
                                || dbgs[i].contains("AddrOf")
                                || dbgs[i].contains("Deref"))
                        {
                            dup = true;
                        }
                    }
                }
                if dup {
                    let callee_name = match callee.as_ref() {
                        Expr::Ident { name, .. } => name.name.clone(),
                        _ => "<?>".to_string(),
                    };
                    let sigs: Vec<String> =
                        args.iter().map(|a| dbg_short_arg(a)).collect();
                    eprintln!(
                        "[DUPARGS] {}: callee={} args=[{}]",
                        func_name,
                        callee_name,
                        sigs.join(", ")
                    );
                }
            }
        }
    }
}

fn dbg_short_arg(e: &crate::ast::Expr) -> String {
    match e {
        crate::ast::Expr::Ident { name, .. } => format!("Id:{}", name.name),
        crate::ast::Expr::AddrOf { e: x, .. } => match x.as_ref() {
            crate::ast::Expr::Ident { name, .. } => format!("&{}", name.name),
            _ => "AddrOf".to_string(),
        },
        crate::ast::Expr::Deref { e: x, .. } => format!("*{:?}", x),
        crate::ast::Expr::Int { v, .. } => format!("I:{}", v),
        crate::ast::Expr::Float { c, .. } => format!("F:{:?}", c),
        _ => "Other".to_string(),
    }
}

/// 读 `/proc/self/statm` 第二字段（常驻页数）。页大小按 4 KB 计（x86_64 默认，本机
/// `getconf PAGESIZE` 实测就是这个值）—— 诊断窗口只看趋势，不值得为跨平台再引依赖。
fn memdbg_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u64>().ok())
                .map(|pages| pages * 4)
        })
        .unwrap_or(0)
}

/// file-scope 的 `register asm` 变量占住了哪些 callee-saved 号（自动分配要让开）。
///
/// 为什么必须让开而不是复用：后端对每个函数都会把 file-scope 绑定重新灌进绑定表，
/// 两个变量共享一个寄存器就是静默错值（§7.15 修的是同一类问题）。
fn file_scope_bound_regs(decls: &[crate::ast::Decl]) -> std::collections::HashSet<u8> {
    let mut out = std::collections::HashSet::new();
    for d in decls {
        if let crate::ast::Decl::Var(v) = d {
            if let Some(rn) = &v.register_name {
                if let Some(r) = crate::ssa::regalloc::pooled_reg_of(rn) {
                    out.insert(r);
                }
            }
        }
    }
    out
}

/// Returns true if the function references any volatile global variable.
fn func_refs_volatile(f: &FuncDecl, volatile_globals: &std::collections::HashSet<String>) -> bool {
    // volatile 形参: 读它同样必须真的访存, 不能被 SSA 复用。
    if f.params.iter().any(|p| p.quals.is_volatile()) {
        return true;
    }
    if let Some(body) = &f.body {
        for item in &body.items {
            if block_item_refs_volatile(item, volatile_globals) {
                return true;
            }
        }
    }
    false
}

fn block_item_refs_volatile(item: &BlockItem, vol: &std::collections::HashSet<String>) -> bool {
    match item {
        BlockItem::Stmt(s) => stmt_refs_volatile(s, vol),
        BlockItem::Decl(Decl::Var(v)) => {
            // 声明的对象本身就是 volatile: 即便初值里没出现 volatile 名字,
            // 后续对它的读也不许被 SSA 复用。
            if v.quals.is_volatile() {
                return true;
            }
            if let Some(init) = &v.init {
                expr_refs_volatile(init, vol)
            } else {
                false
            }
        },
        _ => false,
    }
}

fn stmt_refs_volatile(s: &Stmt, vol: &std::collections::HashSet<String>) -> bool {
    match s {
        Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => expr_refs_volatile(e, vol),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_refs_volatile(cond, vol)
                || stmt_refs_volatile(then_, vol)
                || else_.as_ref().is_some_and(|e| stmt_refs_volatile(e, vol))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_refs_volatile(cond, vol) || stmt_refs_volatile(body, vol)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().is_some_and(|i| stmt_refs_volatile(i, vol))
                || cond.as_ref().is_some_and(|c| expr_refs_volatile(c, vol))
                || step.as_ref().is_some_and(|s| expr_refs_volatile(s, vol))
                || stmt_refs_volatile(body, vol)
        },
        Stmt::Block { b, .. } => b.items.iter().any(|i| block_item_refs_volatile(i, vol)),
        Stmt::Switch { expr, cases, .. } => {
            expr_refs_volatile(expr, vol)
                || cases.iter().any(|c| {
                    c.body
                        .items
                        .iter()
                        .any(|i| block_item_refs_volatile(i, vol))
                })
        },
        // C++ 零成本异常: volatile 引用可以只出现在受保护调用里 (例如 `volatile`
        // 全局当作参)。不下降到这里会让调用点看似不碰 volatile, 参与不该做的移动/
        // 删除 —— volatile 语义要求按可观察顺序执行, 这类判定必须保守。
        Stmt::Invoke { call, .. } => stmt_refs_volatile(call, vol),
        Stmt::EhLandingPad {
            object, selector, ..
        } => expr_refs_volatile(object, vol) || expr_refs_volatile(selector, vol),
        _ => false,
    }
}

fn expr_refs_volatile(e: &Expr, vol: &std::collections::HashSet<String>) -> bool {
    match e {
        Expr::Ident { name, .. } => vol.contains(&name.name),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_refs_volatile(l, vol) || expr_refs_volatile(r, vol)
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. } => expr_refs_volatile(e, vol),
        Expr::Call { callee, args, .. } => {
            expr_refs_volatile(callee, vol) || args.iter().any(|a| expr_refs_volatile(a, vol))
        },
        Expr::Index { arr, idx, .. } => {
            expr_refs_volatile(arr, vol) || expr_refs_volatile(idx, vol)
        },
        Expr::Member { expr, .. } => expr_refs_volatile(expr, vol),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_refs_volatile(cond, vol)
                || expr_refs_volatile(then_, vol)
                || expr_refs_volatile(else_, vol)
        },
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => expr_refs_volatile(e, vol),
        Expr::StmtExpr { block, .. } => {
            block.items.iter().any(|i| block_item_refs_volatile(i, vol))
        },
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            inits.iter().any(|init| match init {
                crate::ast::InitItem::Expr(e) => expr_refs_volatile(e, vol),
                crate::ast::InitItem::Designated { value, .. } => expr_refs_volatile(value, vol),
            })
        },
        _ => false,
    }
}

/// 已知会「返回两次」(returns_twice) 的 libc 函数名。这些函数 (setjmp 族、
/// vfork、getcontext 等) 在第一次返回后, 可能经 longjmp/longjmp 再次从调用点
/// 返回; 编译器必须把它们当作控制流汇合点, 不能在调用点两侧做会让「二次返回」
/// 后寄存器 / 栈态不一致的优化 (否则踩栈崩溃, 见 Lua 的 `luaD_rawrunprotected`)。
///
/// glibc 头在 `__GNUC__ >= 11` 时会给它们打 `__attribute__((__returns_twice__))`,
/// 但这是最后的兜底: 任何头都不保证标记 (dcc 也刚补齐 `__returns_twice__` 拼写),
/// 且自举 / 其它 libc / 交叉头都不会。因此这里按名字全局识别, 与声明上的
/// `returns_twice` 属性取并集, 保证无论头怎么说, SSA 管线都能正确识别。
const KNOWN_RETURNS_TWICE: &[&str] = &[
    "setjmp", "_setjmp", "__setjmp", "__sigsetjmp", "sigsetjmp", "savectx", "savectx_rt",
    "vfork", "getcontext", "setcontext",
];

/// 函数体是否调用了任一 returns_twice 函数。
fn func_contains_rt_call(f: &FuncDecl, rt_names: &std::collections::HashSet<String>) -> bool {
    f.body
        .as_ref()
        .is_some_and(|b| block_contains_rt_call(b, rt_names))
}

fn block_contains_rt_call(b: &Block, rt_names: &std::collections::HashSet<String>) -> bool {
    b.items
        .iter()
        .any(|i| block_item_contains_rt_call(i, rt_names))
}

fn block_item_contains_rt_call(
    item: &BlockItem,
    rt_names: &std::collections::HashSet<String>,
) -> bool {
    match item {
        BlockItem::Stmt(s) => stmt_contains_rt_call(s, rt_names),
        BlockItem::Decl(Decl::Var(v)) => {
            v.init.as_ref().is_some_and(|e| expr_contains_rt_call(e, rt_names))
        },
        _ => false,
    }
}

fn stmt_contains_rt_call(s: &Stmt, rt_names: &std::collections::HashSet<String>) -> bool {
    match s {
        Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => {
            expr_contains_rt_call(e, rt_names)
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_contains_rt_call(cond, rt_names)
                || stmt_contains_rt_call(then_, rt_names)
                || else_.as_ref().is_some_and(|e| stmt_contains_rt_call(e, rt_names))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_contains_rt_call(cond, rt_names) || stmt_contains_rt_call(body, rt_names)
        },
        Stmt::For {
            init, cond, step, body, ..
        } => {
            init.as_ref().is_some_and(|i| stmt_contains_rt_call(i, rt_names))
                || cond.as_ref().is_some_and(|c| expr_contains_rt_call(c, rt_names))
                || step.as_ref().is_some_and(|s| expr_contains_rt_call(s, rt_names))
                || stmt_contains_rt_call(body, rt_names)
        },
        Stmt::Block { b, .. } => block_contains_rt_call(b, rt_names),
        Stmt::Switch { expr, cases, .. } => {
            expr_contains_rt_call(expr, rt_names)
                || cases.iter().any(|c| {
                    c.body
                        .items
                        .iter()
                        .any(|i| block_item_contains_rt_call(i, rt_names))
                })
        },
        // C++ 零成本异常: returns_twice 调用可藏在受保护调用里, 同样要下降。
        Stmt::Invoke { call, .. } => stmt_contains_rt_call(call, rt_names),
        Stmt::EhLandingPad {
            object, selector, ..
        } => expr_contains_rt_call(object, rt_names) || expr_contains_rt_call(selector, rt_names),
        _ => false,
    }
}

fn expr_contains_rt_call(e: &Expr, rt_names: &std::collections::HashSet<String>) -> bool {
    match e {
        Expr::Call { callee, args, .. } => {
            callee_rt_name(callee).map_or(false, |n| rt_names.contains(&n))
                || args.iter().any(|a| expr_contains_rt_call(a, rt_names))
        },
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. } => {
            expr_contains_rt_call(l, rt_names) || expr_contains_rt_call(r, rt_names)
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. } => expr_contains_rt_call(x, rt_names),
        Expr::Index { arr, idx, .. } => {
            expr_contains_rt_call(arr, rt_names) || expr_contains_rt_call(idx, rt_names)
        },
        Expr::Member { expr, .. } => expr_contains_rt_call(expr, rt_names),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_contains_rt_call(cond, rt_names)
                || expr_contains_rt_call(then_, rt_names)
                || expr_contains_rt_call(else_, rt_names)
        },
        Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => expr_contains_rt_call(x, rt_names),
        Expr::StmtExpr { block, .. } => block_contains_rt_call(block, rt_names),
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            inits.iter().any(|init| match init {
                crate::ast::InitItem::Expr(x) => expr_contains_rt_call(x, rt_names),
                crate::ast::InitItem::Designated { value, .. } => {
                    expr_contains_rt_call(value, rt_names)
                },
            })
        },
        _ => false,
    }
}

/// 从调用表达式的 callee 中取出被调函数名 (剥掉 `Cast` 包裹), 用于 returns_twice 判定。
/// 直接调用 `foo(...)` 的 callee 是 `Expr::Ident`; 经过函数指针转换的 `((T)fp)(...)`
/// 仍是 `Ident` 经 `Cast` 包裹, 一并处理。函数指针间接调用 (`(*fp)(...)`) 取不到名字,
/// 保守地不触发退避 —— 这类调用在 Lua/SQLite 等真实项目里不出现于 setjmp 路径。
fn callee_rt_name(callee: &Expr) -> Option<String> {
    match callee {
        Expr::Ident { name, .. } => Some(name.name.clone()),
        Expr::Cast { e, .. } => callee_rt_name(e),
        _ => None,
    }
}

/// 收集函数体内所有「直接调用」的被调函数名 (构建调用图用)。
fn func_direct_callees(f: &FuncDecl) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Some(b) = &f.body {
        collect_callees_block(b, &mut out);
    }
    out
}

fn collect_callees_block(b: &Block, out: &mut std::collections::HashSet<String>) {
    for i in &b.items {
        match i {
            BlockItem::Stmt(s) => collect_callees_stmt(s, out),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &v.init {
                    collect_callees_expr(e, out);
                }
            },
            _ => {}
        }
    }
}

fn collect_callees_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => collect_callees_expr(e, out),
        Stmt::If { cond, then_, else_, .. } => {
            collect_callees_expr(cond, out);
            collect_callees_stmt(then_, out);
            if let Some(e) = else_ {
                collect_callees_stmt(e, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            collect_callees_expr(cond, out);
            collect_callees_stmt(body, out);
        },
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(i) = init {
                collect_callees_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_callees_expr(c, out);
            }
            if let Some(s) = step {
                collect_callees_expr(s, out);
            }
            collect_callees_stmt(body, out);
        },
        Stmt::Block { b, .. } => collect_callees_block(b, out),
        Stmt::Switch { expr, cases, .. } => {
            collect_callees_expr(expr, out);
            for c in cases {
                for i in &c.body.items {
                    collect_callees_block_item(i, out);
                }
            }
        },
        Stmt::Invoke { call, .. } => collect_callees_stmt(call, out),
        Stmt::EhLandingPad { object, selector, .. } => {
            collect_callees_expr(object, out);
            collect_callees_expr(selector, out);
        },
        _ => {}
    }
}

fn collect_callees_block_item(i: &BlockItem, out: &mut std::collections::HashSet<String>) {
    match i {
        BlockItem::Stmt(s) => collect_callees_stmt(s, out),
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(e) = &v.init {
                collect_callees_expr(e, out);
            }
        },
        _ => {}
    }
}

fn collect_callees_expr(e: &Expr, out: &mut std::collections::HashSet<String>) {
    match e {
        Expr::Call { callee, args, .. } => {
            if let Some(n) = callee_rt_name(callee) {
                out.insert(n);
            }
            for a in args {
                collect_callees_expr(a, out);
            }
        },
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. } => {
            collect_callees_expr(l, out);
            collect_callees_expr(r, out);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. } => collect_callees_expr(x, out),
        Expr::Index { arr, idx, .. } => {
            collect_callees_expr(arr, out);
            collect_callees_expr(idx, out);
        },
        Expr::Member { expr, .. } => collect_callees_expr(expr, out),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_callees_expr(cond, out);
            collect_callees_expr(then_, out);
            collect_callees_expr(else_, out);
        },
        Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => collect_callees_expr(x, out),
        Expr::StmtExpr { block, .. } => collect_callees_block(block, out),
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            for init in inits {
                match init {
                    crate::ast::InitItem::Expr(x) => collect_callees_expr(x, out),
                    crate::ast::InitItem::Designated { value, .. } => {
                        collect_callees_expr(value, out)
                    },
                }
            }
        },
        _ => {}
    }
}

/// 计算 `returns_twice` 影响域 (cone): 直接含 returns_twice 调用的函数, 以及 (传递地)
/// 调用这类函数的所有函数。这些函数要么自身有「二次返回」控制流汇合点, 要么身处其
/// 调用链中参与非局部控制流, 整条链都退出 SSA 管线走保守的 AST 路径最稳妥。
fn compute_rt_cone(
    prog: &Program,
    rt_names: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    // seed: 直接含 returns_twice 调用的函数
    let mut cone: std::collections::HashSet<String> = prog
        .decls
        .iter()
        .filter_map(|d| {
            if let Decl::Func(f) = d {
                if func_contains_rt_call(f, rt_names) {
                    Some(f.name.name.clone())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // 调用图: caller -> 直接被调函数名集合
    let mut graph: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for d in &prog.decls {
        if let Decl::Func(f) = d {
            graph.insert(f.name.name.clone(), func_direct_callees(f));
        }
    }

    // 反向传播: 若 g ∈ cone 且 f 直接调用 g, 则 f ∈ cone (直到不动点)
    let mut changed = true;
    while changed {
        changed = false;
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if cone.contains(&f.name.name) {
                    continue;
                }
                if let Some(callees) = graph.get(&f.name.name) {
                    if callees.iter().any(|c| cone.contains(c)) {
                        cone.insert(f.name.name.clone());
                        changed = true;
                    }
                }
            }
        }
    }
    cone
}

/// Qualifier-aware optimization: const/volatile/restrict utilization.
///
/// - **const**: Collect global `const` variables and warn on stores to them.
///   Const globals are placed in read-only sections by the linker, so stores
///   would cause runtime faults.
/// - **volatile**: Already handled above - functions accessing volatile globals
///   skip SSA optimization to preserve load/store ordering.
/// - **restrict**: Function parameters with restrict qualifier indicate
///   non-aliasing pointers. This information is available for future alias
///   analysis improvements (currently alias_analysis in src/ssa/passes.rs does
///   not use restrict hints).
fn qualifier_aware_optimization(module: &mut Module) {
    let prog = module.source();
    // Collect const global variable names
    let const_globals: std::collections::HashSet<String> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Var(v) if v.quals.is_const() => Some(v.name.name.clone()),
            _ => None,
        })
        .collect();
    // Check for stores to const globals and emit warnings
    if !const_globals.is_empty() {
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if let Some(body) = &f.body {
                    check_stores_to_const_globals(body, &const_globals);
                }
            }
        }
    }
}

/// Check for stores to const global variables and emit warnings.
fn check_stores_to_const_globals(block: &Block, const_globals: &std::collections::HashSet<String>) {
    for item in &block.items {
        match item {
            BlockItem::Stmt(s) => check_stmt_for_const_stores(s, const_globals),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &v.init {
                    check_expr_for_const_stores(init, const_globals);
                }
            },
            _ => {},
        }
    }
}

fn check_stmt_for_const_stores(stmt: &Stmt, const_globals: &std::collections::HashSet<String>) {
    match stmt {
        Stmt::Expr { e, .. } => check_expr_for_const_stores(e, const_globals),
        Stmt::Return { val: Some(e), .. } => check_expr_for_const_stores(e, const_globals),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            check_expr_for_const_stores(cond, const_globals);
            check_stmt_for_const_stores(then_, const_globals);
            if let Some(e) = else_ {
                check_stmt_for_const_stores(e, const_globals);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            check_expr_for_const_stores(cond, const_globals);
            check_stmt_for_const_stores(body, const_globals);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(init_stmt) = init {
                check_stmt_for_const_stores(init_stmt, const_globals);
            }
            if let Some(cond_expr) = cond {
                check_expr_for_const_stores(cond_expr, const_globals);
            }
            if let Some(step_expr) = step {
                check_expr_for_const_stores(step_expr, const_globals);
            }
            check_stmt_for_const_stores(body, const_globals);
        },
        Stmt::Block { b, .. } => check_stores_to_const_globals(b, const_globals),
        Stmt::Switch { expr, cases, .. } => {
            check_expr_for_const_stores(expr, const_globals);
            for case in cases {
                check_stores_to_const_globals(&case.body, const_globals);
            }
        },
        _ => {},
    }
}

fn check_expr_for_const_stores(expr: &Expr, const_globals: &std::collections::HashSet<String>) {
    match expr {
        Expr::Assign { l, .. } => {
            // Check if assigning to a const global
            // Note: Warning emission would require diagnostic infrastructure
            // integration. For now, we just detect the pattern.
            if let Expr::Ident { name, .. } = l.as_ref() {
                if const_globals.contains(&name.name) {
                    // Store to const global detected - would emit warning
                    // if diagnostic infrastructure was integrated here
                }
            }
        },
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            check_expr_for_const_stores(l, const_globals);
            check_expr_for_const_stores(r, const_globals);
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. } => {
            check_expr_for_const_stores(e, const_globals);
        },
        Expr::Call { callee, args, .. } => {
            check_expr_for_const_stores(callee, const_globals);
            for arg in args {
                check_expr_for_const_stores(arg, const_globals);
            }
        },
        Expr::Index { arr, idx, .. } => {
            check_expr_for_const_stores(arr, const_globals);
            check_expr_for_const_stores(idx, const_globals);
        },
        Expr::Member { expr, .. } => check_expr_for_const_stores(expr, const_globals),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            check_expr_for_const_stores(cond, const_globals);
            check_expr_for_const_stores(then_, const_globals);
            check_expr_for_const_stores(else_, const_globals);
        },
        Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => {
            // Increment/decrement on const global is also an error
            if let Expr::Ident { name, .. } = e.as_ref() {
                if const_globals.contains(&name.name) {
                    // Increment/decrement of const global detected
                }
            }
            check_expr_for_const_stores(e, const_globals);
        },
        Expr::StmtExpr { block, .. } => check_stores_to_const_globals(block, const_globals),
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            for init in inits {
                match init {
                    crate::ast::InitItem::Expr(e) => check_expr_for_const_stores(e, const_globals),
                    crate::ast::InitItem::Designated { value, .. } => {
                        check_expr_for_const_stores(value, const_globals)
                    },
                }
            }
        },
        _ => {},
    }
}

/// 检测函数是否包含 SSA→AST 重建当前无法正确处理的模式:
///   - asm goto (非结构化控制流)
///   - 数组类型带初始化式 (SSA 无法表示复合字面量)
///
/// 取址/解引用/索引已由 SSA 指令集 (AddrOf/Load/Store/IndexLoad/IndexStore)
/// 完整支持, 不再跳过。
fn func_has_unsafe_ssa_pattern(f: &FuncDecl, allow_static_locals: bool) -> bool {
    f.body
        .as_ref()
        .is_some_and(|b| block_has_unsafe_ssa_pattern(b, allow_static_locals))
}

fn block_has_unsafe_ssa_pattern(b: &Block, allow_static_locals: bool) -> bool {
    b.items.iter().any(|item| match item {
        // analysis-only 通道（含 goto / computed goto 的函数，不重建、不产代码）上，
        // `static` 局部不构成风险：分配器的候选判据把 `is_static` 一向排除在
        // `plain_local` 之外（`regalloc::scan_block`），它既拿不到家也不会改变帧布局，
        // 而整棵 AST 原封不动。下面那条 `static` 判据的真正理由是**重建**会丢掉
        // `is_static`（重建后的声明变成普通栈局部）—— 这条通道不重建。
        // 典型样本就是解释器主循环里的
        // `static const void *const disptab[NUM_OPCODES] = { &&L_OP_MOVE, ... };`
        // —— 它是 `luaV_execute` 进不了 SSA 的唯一原因（同时命中下面两条：static、
        // 数组带初始化式）。
        BlockItem::Decl(Decl::Var(v)) if allow_static_locals && v.is_static => false,
        BlockItem::Decl(Decl::Var(v)) => {
            // 数组类型带初始化式 (复合字面量 SSA 无法表示)
            (matches!(v.ty, crate::ast::Type::Array(_, _))
                && v.init.as_ref().is_some())
            // static 局部变量跨调用持久, SSA 无法正确建模
            || v.is_static
            // `__attribute__((cleanup(f)))` 是块作用域上的一个**隐式调用**，而 IR 里没有它
            // 的表示：实测 `rebuild_block` 重建声明时写的是 `cleanup_fn: None`
            // （`ssa/rebuild.rs:183`），于是 -O2/-O3 把清理**整个丢掉**（普通块与语句
            // 表达式两类作用域都受影响），同一个源在 -O0/-O1 与 -O2 给出两个答案。
            // 这里让整函数退回 AST 路径（与同列的 static/extern/VLA 同一处理口径），
            // 而不是在 IR 里新增一个副作用调用 —— 后者是另一个数量级的改动。
            || v.cleanup_fn.is_some()
            // **VLA / VM 聚合（带运行时尺寸的对象）整函数退回 AST 路径**。
            // IR 里没有「运行时尺寸对象」这个表示：`collect_local_decls` 存的是名字
            // 与类型快照，`rebuild_block` 重建后那个对象在两张表（`var_map` /
            // `vla_offsets`）里的身份不再是发射期那条判据 —— 实测：
            //   `int n=c+3; long a[n]; …; a[2]=7; sizeof a/8 + a[2] - 12`
            //     gcc / dcc -O0 / -O1 = 255，dcc -O2 / -O3 = **252**（`sizeof a` 读回 0，
            //     元素访问与写各自落进描述符区，自洽但错）；
            //   `struct { char data[size]; } a; …; probe(1, a)`
            //     -O2/-O3 **SIGSEGV(139)**（调用侧 `mov -104(%r13),%rcx; mov (%rcx),%rax`
            //     解引用一个被数据踩掉的描述符槽）。
            // 与 `vla.c` 那种「写完再读回」的夹具不同，上面两条把 `sizeof`/传参与
            // 元素访问放在一起，才把「基址错但自洽」照出来。
            // 真正的修法是让 IR 能表达运行时尺寸对象（尺寸槽、运行时 stride、
            // 地址身份三件），那是另一个数量级的改动；在此之前正确的口径与
            // `static`/`cleanup`/嵌套函数同一档：IR 表达不了的就留在 AST 上。
            || v.ty.has_vla()
        },
        // 嵌套函数：重建后的体里**没有** `Decl::Func`（被前面的降低步骤按帧偏移另存），
        // 于是 -O2 产物里它的定义直接消失。实测最小复现
        // `int main(void){ int id(void){ return 3; } return id()==3?0:1; }`：
        // -O0 链得过、-O2 `undefined reference to 'id'`。§7.20 为寄存器分配记过同一事实
        // （那边是「不能把被嵌套函数访达的变量绑进寄存器」），这边把整函数退出 SSA。
        BlockItem::Decl(Decl::Func(_)) => true,
        BlockItem::Stmt(s) => stmt_has_unsafe_ssa_pattern(s, allow_static_locals),
        _ => false,
    })
}

fn stmt_has_unsafe_ssa_pattern(s: &Stmt, allow_static_locals: bool) -> bool {
    match s {
        // asm goto 包含无法 SSA 化的非结构化控制流
        Stmt::AsmGoto { .. } => true,
        Stmt::If { then_, else_, .. } => {
            stmt_has_unsafe_ssa_pattern(then_, allow_static_locals)
                || else_
                    .as_ref()
                    .is_some_and(|e| stmt_has_unsafe_ssa_pattern(e, allow_static_locals))
        },
        Stmt::Block { b, .. } => block_has_unsafe_ssa_pattern(b, allow_static_locals),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            stmt_has_unsafe_ssa_pattern(body, allow_static_locals)
        },
        Stmt::For { init, body, .. } => {
            init.as_ref()
                .is_some_and(|i| stmt_has_unsafe_ssa_pattern(i, allow_static_locals))
                || stmt_has_unsafe_ssa_pattern(body, allow_static_locals)
        },
        Stmt::Switch { cases, .. } => cases
            .iter()
            .any(|c| block_has_unsafe_ssa_pattern(&c.body, allow_static_locals)),
        // 标号语句的体里也能带声明（`out: struct {int d[n];} b;`）：不递归就与
        // 上面那条注释里「只看顶层 item 会漏掉语句表达式里的声明」同一病灶。
        Stmt::Label { stmt, .. } => stmt_has_unsafe_ssa_pattern(stmt, allow_static_locals),
        // 语句表达式 `({...})` 里的声明也是声明：只看顶层 item 会漏掉它（实测：
        // `({ __attribute__((cleanup(cb))) long c = 4; ... })` 的清理在 -O2 静默消失）。
        Stmt::Expr { e, .. } => expr_has_unsafe_ssa_pattern(e, allow_static_locals),
        _ => false,
    }
}

/// 表达式树里有没有「只能待在 AST 上」的形态（目前就是语句表达式块里的声明）。
///
/// 与 `ast_asm::walk_expr` 同构地穷举写：新增 `Expr` 变体会直接编不过，而不是静默
/// 漏掉一个子树（漏一个就是上面那条注释里那个静默错值的又一个版本）。
fn expr_has_unsafe_ssa_pattern(e: &Expr, allow_static_locals: bool) -> bool {
    use crate::ast::Expr as E;
    match e {
        E::Int { .. }
        | E::Float { .. }
        | E::ComplexLit { .. }
        | E::ComplexIntLit { .. }
        | E::String { .. }
        | E::Char { .. }
        | E::SizeofTy { .. }
        | E::AlignofTy { .. }
        | E::Offsetof { .. }
        | E::LabelAddr { .. }
        | E::VaArgPack { .. }
        | E::Ident { .. } => false,
        E::Unary { e: x, .. }
        | E::Cast { e: x, .. }
        | E::AddrOf { e: x, .. }
        | E::Deref { e: x, .. }
        | E::Real { e: x, .. }
        | E::Imag { e: x, .. }
        | E::SizeofExpr { e: x, .. }
        | E::AlignofExpr { e: x, .. }
        | E::PostInc { e: x, .. }
        | E::PostDec { e: x, .. }
        | E::PreInc { e: x, .. }
        | E::PreDec { e: x, .. } => expr_has_unsafe_ssa_pattern(x, allow_static_locals),
        E::Member { expr: x, .. } => expr_has_unsafe_ssa_pattern(x, allow_static_locals),
        E::Binop { l, r, .. }
        | E::Assign { l, r, .. }
        | E::Comma { l, r, .. }
        | E::Index { arr: l, idx: r, .. } => {
            expr_has_unsafe_ssa_pattern(l, allow_static_locals)
                || expr_has_unsafe_ssa_pattern(r, allow_static_locals)
        },
        E::Cond {
            cond, then_, else_, ..
        } => {
            expr_has_unsafe_ssa_pattern(cond, allow_static_locals)
                || expr_has_unsafe_ssa_pattern(then_, allow_static_locals)
                || expr_has_unsafe_ssa_pattern(else_, allow_static_locals)
        },
        E::Call { callee, args, .. } => {
            expr_has_unsafe_ssa_pattern(callee, allow_static_locals)
                || args
                    .iter()
                    .any(|a| expr_has_unsafe_ssa_pattern(a, allow_static_locals))
        },
        E::StmtExpr { block, .. } => block_has_unsafe_ssa_pattern(block, allow_static_locals),
        E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
            inits.iter().any(|it| match it {
                crate::ast::InitItem::Expr(x) => {
                    expr_has_unsafe_ssa_pattern(x, allow_static_locals)
                },
                crate::ast::InitItem::Designated { value, .. } => {
                    expr_has_unsafe_ssa_pattern(value, allow_static_locals)
                },
            })
        },
        E::VaArg { ap, .. } => expr_has_unsafe_ssa_pattern(ap, allow_static_locals),
        E::AsmExpr {
            outputs, inputs, ..
        } => {
            outputs
                .iter()
                .any(|o| expr_has_unsafe_ssa_pattern(o, allow_static_locals))
                || inputs
                    .iter()
                    .any(|i| expr_has_unsafe_ssa_pattern(i, allow_static_locals))
        },
    }
}

/// M16: 跨函数 DCE - 基于 call graph 标记不可达 internal static 函数
fn cross_function_dce(module: &mut Module) {
    let prog = module.source_mut();
    let mut funcs: Vec<FuncDecl> = Vec::new();
    let mut others: Vec<Decl> = Vec::new();
    let mut extra_roots = Vec::new();
    for d in prog.decls.drain(..) {
        match d {
            Decl::Func(f) => {
                // `__attribute__((copy(fn)))` 在产物里是 `.set 别名, 目标`；目标必须当根，
                // 否则它因「没人直接调用」被 DCE 吃掉，剩下一条指向上定义已消失的
                // `.set` —— 链接期只报 `undefined reference to 别名`，完全看不出真正被
                // 删的是谁。实测（-O0 对、-O2/-O3 链接失败）：
                //     static int _internal_impl(int a,int b){return a+b;}
                //     int public_api(int,int) __attribute__((copy("_internal_impl")));
                if let Some(target) = f.alias_name.clone() {
                    extra_roots.push(target);
                }
                funcs.push(f);
            },
            Decl::Var(v) => {
                if let Some(init) = &v.init {
                    collect_global_func_roots(init, &mut extra_roots);
                }
                others.push(Decl::Var(v));
            },
            Decl::GlobalAsm {
                ref outputs,
                ref inputs,
                ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    collect_global_func_roots(e, &mut extra_roots);
                }
                others.push(d);
            },
            other => others.push(other),
        }
    }
    crate::ssa::passes::cross_function_dce_with_roots(&mut funcs, true, &extra_roots);
    let mut all: Vec<Decl> = others;
    for f in funcs {
        all.push(Decl::Func(f));
    }
    prog.decls = all;
}

fn collect_global_func_roots(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Ident { name, ty, .. } => {
            if matches!(ty, crate::ast::Type::Func(_, _)) {
                out.push(name.name.clone());
            }
        },
        Expr::AddrOf { e, .. } => collect_global_func_roots(e, out),
        Expr::Cast { e, .. }
        | Expr::Unary { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Deref { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::VaArg { ap: e, .. } => collect_global_func_roots(e, out),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_global_func_roots(l, out);
            collect_global_func_roots(r, out);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_global_func_roots(cond, out);
            collect_global_func_roots(then_, out);
            collect_global_func_roots(else_, out);
        },
        Expr::Index { arr, idx, .. } => {
            collect_global_func_roots(arr, out);
            collect_global_func_roots(idx, out);
        },
        Expr::Member { expr, .. } => collect_global_func_roots(expr, out),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                match init {
                    crate::ast::InitItem::Expr(e) => collect_global_func_roots(e, out),
                    crate::ast::InitItem::Designated { value, .. } => {
                        collect_global_func_roots(value, out)
                    },
                }
            }
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                collect_global_func_roots(e, out);
            }
        },
        _ => {},
    }
}

fn fold_constants(module: &mut Module) {
    let prog = module.source_mut();
    // FENV_ACCESS ON 时浮点运算依赖运行时舍入模式, 编译期折叠会改变语义
    // (C99 F.8): 折叠器跳过浮点二元运算折叠.
    let fenv_access = prog.fenv_access;
    fold_program(prog, fenv_access);
}

fn fold_program(prog: &mut Program, fenv_access: bool) {
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                fold_block(&mut body.items, fenv_access);
            }
        }
    }
}

fn fold_block(items: &mut [BlockItem], fenv_access: bool) {
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => fold_stmt(s, fenv_access),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    *init = fold_expr(init.clone(), fenv_access);
                }
            },
            _ => {},
        }
    }
}

fn fold_stmt(s: &mut Stmt, fenv_access: bool) {
    match s {
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_stmt(then_, fenv_access);
            if let Some(e) = else_ {
                fold_stmt(e, fenv_access);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_stmt(body, fenv_access);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone(), fenv_access);
            }
            if let Some(st) = step {
                *st = fold_expr(st.clone(), fenv_access);
            }
            if let Some(i) = init {
                fold_stmt(i, fenv_access);
            }
            fold_stmt(body, fenv_access);
        },
        Stmt::Return { val: Some(e), .. } => {
            *e = fold_expr(e.clone(), fenv_access);
        },
        Stmt::Expr { e, .. } => *e = fold_expr(e.clone(), fenv_access),
        Stmt::Block { b, .. } => fold_block(&mut b.items, fenv_access),
        _ => {},
    }
}

fn fold_expr(e: Expr, fenv_access: bool) -> Expr {
    let e = match e {
        Expr::Binop { op, l, r, span, ty } => {
            let l = Box::new(fold_expr(*l, fenv_access));
            let r = Box::new(fold_expr(*r, fenv_access));
            // 整数常量折叠: 优先使用 const_eval (类型感知, 正确处理
            // 无符号类型宽度和溢出语义).
            if let (Expr::Int { .. }, Expr::Int { .. }) = (l.as_ref(), r.as_ref()) {
                let binop_expr = Expr::Binop {
                    op,
                    l: l.clone(),
                    r: r.clone(),
                    span: span.clone(),
                    ty: ty.clone(),
                };
                if let Ok(v) = const_eval::eval_full(&binop_expr, fenv_access) {
                    if let Some(iv) = v.as_int() {
                        return Expr::Int { v: iv, span, ty };
                    }
                }
            }
            // FENV_ACCESS ON: 不折叠浮点常量运算 (舍入模式可能被 fesetround
            // 在运行时改变, 编译期折叠将按默认舍入求值, 语义错误).
            if !fenv_access {
                if let (Expr::Float { c: lc, .. }, Expr::Float { c: rc, .. }) =
                    (l.as_ref(), r.as_ref())
                {
                    // LongDouble 不做编译期折叠：宿主无 80-bit 运算，
                    // 留给后端 x87 运行时按目标舍入模式求值。
                    if lc.is_long_double() || rc.is_long_double() {
                        // fall through, keep original expression
                    } else if let Some(v) = eval_float_binop(op, lc.as_f64(), rc.as_f64()) {
                        // 结果精度必须匹配目标类型: Float 结果截为 f32
                        let c = if matches!(ty, crate::ast::Type::Float) {
                            crate::ast::FloatConst::F32(v as f32)
                        } else {
                            crate::ast::FloatConst::F64(v)
                        };
                        return Expr::Float { c, span, ty };
                    }
                }
            }
            Expr::Binop { op, l, r, span, ty }
        },
        Expr::Unary { op, e, span, ty } => {
            let e = Box::new(fold_expr(*e, fenv_access));
            // 整数一元运算折叠: 使用 const_eval (类型感知, 正确处理
            // BitNot 的类型宽度掩码).
            if let Expr::Int { .. } = e.as_ref() {
                let unary_expr = Expr::Unary {
                    op,
                    e: e.clone(),
                    span: span.clone(),
                    ty: ty.clone(),
                };
                if let Ok(v) = const_eval::eval_full(&unary_expr, fenv_access) {
                    if let Some(iv) = v.as_int() {
                        return Expr::Int { v: iv, span, ty };
                    }
                }
            }
            Expr::Unary { op, e, span, ty }
        },
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if name.name == "__builtin_constant_p" && args.len() == 1 {
                    // 参数本身是编译期常量 (字面量或可完全折叠的表达式) 时直接折 1,
                    // 与 GCC 及后端兜底一致 (`constp.c`/`builtins.c` 等夹具)。
                    // 但变量参数**不在此急切折 0**: 留给后续 -O2 的 SCCP —— 参数被
                    // 常量传播成常量后这里应折 1 (修复 modref-2); 若没有 SCCP
                    // (如 -O0) 则保留调用, 由后端兜底 (变量参数 → 0)。
                    if const_eval::is_constant(&args[0]) {
                        return Expr::Int { v: 1, span, ty };
                    }
                    // 非常量参数: 保留调用, 不折 0.
                }
            }
            Expr::Call {
                callee: Box::new(fold_expr(*callee, fenv_access)),
                args: args
                    .into_iter()
                    .map(|a| fold_expr(a, fenv_access))
                    .collect(),
                span,
                ty,
            }
        },
        other => other,
    };
    if let Ok(v) = const_eval::eval(&e) {
        if let Expr::Int { span, ty, .. } = &e {
            return Expr::Int {
                v,
                span: span.clone(),
                ty: ty.clone(),
            };
        }
    }
    e
}

fn eval_binop(op: BinOp, l: i128, r: i128) -> Option<i128> {
    Some(match op {
        BinOp::Add => l.checked_add(r)?,
        BinOp::Sub => l.checked_sub(r)?,
        BinOp::Mul => l.checked_mul(r)?,
        BinOp::Div => {
            if r == 0 {
                return None;
            } else {
                l / r
            }
        },
        BinOp::Mod => {
            if r == 0 {
                return None;
            } else {
                l % r
            }
        },
        BinOp::And => l & r,
        BinOp::Or => l | r,
        BinOp::Xor => l ^ r,
        BinOp::Shl => l << (r as u32),
        BinOp::Shr => l >> (r as u32),
        _ => return None,
    })
}

fn eval_unary(op: crate::ast::UnOp, v: i128) -> Option<i128> {
    Some(match op {
        crate::ast::UnOp::Neg => v.checked_neg()?,
        crate::ast::UnOp::Not => {
            if v == 0 {
                1
            } else {
                0
            }
        },
        crate::ast::UnOp::BitNot => !v,
        _ => return None,
    })
}

fn eval_float_binop(op: BinOp, l: f64, r: f64) -> Option<f64> {
    Some(match op {
        BinOp::Add => l + r,
        BinOp::Sub => l - r,
        BinOp::Mul => l * r,
        BinOp::Div => {
            if r == 0.0 {
                return None;
            } else {
                l / r
            }
        },
        BinOp::Mod => {
            if r == 0.0 {
                return None;
            } else {
                // C fmod semantics: l - trunc(l/r) * r
                // Rust f64 % f64 matches C fmod()
                l % r
            }
        },
        _ => return None,
    })
}

fn prune_dead_stores(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                prune_block(&mut body.items);
            }
        }
    }
}

fn prune_block(items: &mut Vec<BlockItem>) {
    items.retain(
        |item| !matches!(item, BlockItem::Stmt(Stmt::Expr { e, .. }) if is_side_effect_free(e)),
    );
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => prune_stmt(s),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &mut f.body {
                    prune_block(&mut body.items);
                }
            },
            _ => {},
        }
    }
}

fn prune_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            prune_stmt(then_);
            if let Some(e) = else_ {
                prune_stmt(e);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => prune_stmt(body),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prune_stmt(i);
            }
            prune_stmt(body);
        },
        Stmt::Block { b, .. } => prune_block(&mut b.items),
        _ => {},
    }
}

/// -O2: 常量条件分支折叠（DCE 的一种）。
fn fold_constant_branches(module: &mut Module) {
    let prog = module.source_mut();
    let fenv_access = prog.fenv_access;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                fold_branches_block(&mut body.items, fenv_access);
            }
        }
    }
}

/// 语句（递归）内是否含 Label。goto 可跳入常量死分支中的标签,
/// 折叠时若丢弃标签会导致 codegen "unresolved label" 崩溃。
fn stmt_has_label(s: &Stmt) -> bool {
    match s {
        Stmt::Label { .. } => true,
        Stmt::Block { b, .. } => block_has_label(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_label(then_) || else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            stmt_has_label(body)
        },
        Stmt::Switch { cases, .. } => cases.iter().any(|c| block_has_label(&c.body)),
        _ => false,
    }
}

fn block_has_label(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_label(s),
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().map(block_has_label).unwrap_or(false),
        _ => false,
    })
}

fn fold_branches_block(items: &mut Vec<BlockItem>, fenv_access: bool) {
    let mut i = 0;
    while i < items.len() {
        let fold_if = matches!(
            items.get(i),
            Some(BlockItem::Stmt(Stmt::If { .. })) if const_eval::eval(
                match items.get(i) {
                    Some(BlockItem::Stmt(Stmt::If { cond, .. })) => cond,
                    _ => unreachable!(),
                }
            ).is_ok()
        );
        if fold_if {
            let item = items.remove(i);
            if let BlockItem::Stmt(Stmt::If {
                cond,
                then_,
                else_,
                span,
                ..
            }) = item
            {
                let v = const_eval::eval(&cond).unwrap_or(0);
                // 被丢弃的分支内含 Label 时不能折叠: goto 仍可跳入, 删除会丢标签
                let has_label = if v != 0 {
                    else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
                } else {
                    stmt_has_label(&then_)
                };
                if has_label {
                    items.insert(
                        i,
                        BlockItem::Stmt(Stmt::If {
                            cond,
                            then_,
                            else_,
                            span,
                        }),
                    );
                    i += 1;
                    continue;
                }
                let taken = if v != 0 {
                    *then_
                } else {
                    else_.map(|e| *e).unwrap_or(Stmt::Block {
                        b: Block {
                            items: vec![],
                            span: span.clone(),
                        },
                        span: span.clone(),
                    })
                };
                match taken {
                    Stmt::Block { mut b, .. } => {
                        fold_branches_block(&mut b.items, fenv_access);
                        for it in b.items.into_iter().rev() {
                            items.insert(i, it);
                        }
                        continue;
                    },
                    other => items.insert(i, BlockItem::Stmt(other)),
                }
            }
        } else if let BlockItem::Stmt(s) = &mut items[i] {
            fold_branches_stmt(s, fenv_access);
        }
        i += 1;
    }
}

fn fold_branches_stmt(s: &mut Stmt, fenv_access: bool) {
    match s {
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_branches_stmt(then_, fenv_access);
            if let Some(e) = else_ {
                fold_branches_stmt(e, fenv_access);
            }
        },
        Stmt::While {
            cond, body, span, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            if let Ok(v) = const_eval::eval(cond) {
                if v == 0 && !stmt_has_label(body) {
                    *s = Stmt::Block {
                        b: Block {
                            items: vec![],
                            span: span.clone(),
                        },
                        span: span.clone(),
                    };
                    return;
                }
            }
            fold_branches_stmt(body, fenv_access);
        },
        Stmt::Block { b, .. } => fold_branches_block(&mut b.items, fenv_access),
        Stmt::For {
            init,
            cond,
            body,
            span,
            ..
        } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone(), fenv_access);
                if let Ok(v) = const_eval::eval(c) {
                    if v == 0 && !stmt_has_label(body) {
                        if let Some(i) = init.take() {
                            *s = Stmt::Expr {
                                e: stmt_to_void_expr(*i),
                                span: span.clone(),
                            };
                        } else {
                            *s = Stmt::Block {
                                b: Block {
                                    items: vec![],
                                    span: span.clone(),
                                },
                                span: span.clone(),
                            };
                        }
                        return;
                    }
                }
            }
            if let Some(i) = init {
                fold_branches_stmt(i, fenv_access);
            }
            fold_branches_stmt(body, fenv_access);
        },
        _ => {},
    }
}

fn items_clear_for_stmt(s: &mut Stmt) {
    let span = s.span().clone();
    *s = Stmt::Block {
        b: Block {
            items: vec![],
            span: span.clone(),
        },
        span,
    };
}

fn stmt_to_void_expr(s: Stmt) -> Expr {
    match s {
        Stmt::Expr { e, .. } => e,
        Stmt::Block { .. } => Expr::Int {
            v: 0,
            span: Span::point("", 0, 0),
            ty: crate::ast::Type::Int,
        },
        _ => Expr::Int {
            v: 0,
            span: Span::point("", 0, 0),
            ty: crate::ast::Type::Int,
        },
    }
}

/// -O2: return 后不可达语句删除。
fn eliminate_unreachable(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                dce_block(&mut body.items);
            }
        }
    }
}

fn dce_block(items: &mut Vec<BlockItem>) {
    let mut stopped = false;
    items.retain(|item| {
        if stopped {
            // G2: `Stmt::Label { stmt }` 只把紧随标签的**一条语句**纳入自己，
            // 标签之后到下一个 Label/terminator 之间的顶层 item（典型形态是
            // `out: <body>; return s;` 里的 `return s;`）仍然是「跳到 out 之后
            // 可达」的路径。旧实现只保 Label 本身、把中间的 Return 全清掉 ⇒
            // `goto out` 从 `out:` 直落进下一个标签、再从函数尾 fall-off，
            // `%eax` 只剩最后一条对 `s` 的写，实测 `out:` 少 3 变成少 3+5。
            // 正确的语义是：Label 是跳入入口，遇到它就把 stopped 复位；此后
            // 的 item 保留到本 block 末尾或下一个顶层 Return（再触发 stopped）。
            if matches!(item, BlockItem::Stmt(Stmt::Label { .. })) {
                stopped = false;
                return true;
            }
            return false;
        }
        if let BlockItem::Stmt(Stmt::Return { .. }) = item {
            stopped = true;
        }
        true
    });
    for item in items.iter_mut() {
        if let BlockItem::Stmt(s) = item {
            dce_stmt(s);
        }
    }
}

fn dce_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            dce_block_in_stmt(then_);
            if let Some(e) = else_ {
                dce_block_in_stmt(e);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            dce_block_in_stmt(body);
        },
        Stmt::Block { b, .. } => dce_block(&mut b.items),
        _ => {},
    }
}

fn dce_block_in_stmt(s: &mut Stmt) {
    if let Stmt::Block { b, .. } = s {
        dce_block(&mut b.items);
    } else {
        dce_stmt(s);
    }
}

/// 内联入口: 收集模块内所有函数定义, 然后逐函数内联.
///
/// `level` 控制内联激进程度:
///   - O0: 仅 `always_inline` / `flatten`
///   - O1-O2: 同 O0, 但 hot caller (profile-guided) 享受放宽阈值
///   - O3: 额外启发式小型 static 零参函数 (cost ≤ 20 AST nodes)
fn function_inline(module: &mut Module, level: u8) {
    let prog = module.source_mut();
    let fenv_access = prog.fenv_access;
    let funcs: HashMap<String, FuncDecl> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Func(f) if f.body.is_some() => Some((f.name.name.clone(), f.clone())),
            _ => None,
        })
        .collect();
    if funcs.is_empty() {
        return;
    }
    // 调用点数、asm 文本引用的名字与函数表一起打包（见 `InlineTable::build` 的理由）。
    let table = InlineTable::build(funcs, prog);
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let caller_flatten = f.is_flatten;
            let caller_is_hot = f.is_hot;
            if let Some(body) = &mut f.body {
                inline_calls_in_block(
                    body,
                    &table,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    0,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Inline cost model
//
// Estimates the code-size impact of inlining a function call.
// Used by the O3 heuristic path and reserved for future O1/O2 cost-driven
// inlining.
//
// The model is intentionally simple (AST node counting) since dcc operates
// on the pre-lowering AST, not a CFG/SSA.  The goal is to prevent
// code-size blow-up from inlining large or complex callees while still
// capturing the common kernel pattern of small accessor/macro functions.
// ---------------------------------------------------------------------------

/// Maximum cost threshold for speculative inlining at O3.
/// Computed as: body_expr_nodes + param_count * avg_arg_nodes.
const INLINE_COST_THRESHOLD: usize = 20;

/// Count AST nodes in an expression (recursive).
fn expr_node_count(e: &Expr) -> usize {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::SizeofExpr { .. }
        | Expr::AlignofTy { .. }
        | Expr::AlignofExpr { .. }
        | Expr::Offsetof { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::VaArgPack { .. }
        | Expr::LabelAddr { .. } => 1,
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::Deref { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. } => 1 + expr_node_count(e),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. }
        | Expr::Comma { l, r, .. } => 1 + expr_node_count(l) + expr_node_count(r),
        Expr::Cond {
            cond, then_, else_, ..
        } => 1 + expr_node_count(cond) + expr_node_count(then_) + expr_node_count(else_),
        Expr::Call { callee, args, .. } => {
            1 + expr_node_count(callee) + args.iter().map(expr_node_count).sum::<usize>()
        },
        Expr::Member { expr, .. } => 1 + expr_node_count(expr),
        Expr::VaArg { ap, .. } => 1 + expr_node_count(ap),
        Expr::StmtExpr { block, .. } => 1 + block.items.iter().map(item_node_count).sum::<usize>(),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            1 + inits.iter().map(init_node_count).sum::<usize>()
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            1 + outputs
                .iter()
                .chain(inputs.iter())
                .map(expr_node_count)
                .sum::<usize>()
        },
    }
}

fn item_node_count(item: &BlockItem) -> usize {
    match item {
        BlockItem::Stmt(s) => stmt_node_count(s),
        BlockItem::Decl(Decl::Var(v)) => 1 + v.init.as_ref().map_or(0, expr_node_count),
        BlockItem::Decl(Decl::Func(f)) => {
            1 + f
                .body
                .as_ref()
                .map_or(0, |b| b.items.iter().map(item_node_count).sum::<usize>())
        },
        _ => 1,
    }
}

fn stmt_node_count(s: &Stmt) -> usize {
    match s {
        Stmt::Return { val, .. } => 1 + val.as_ref().map_or(0, expr_node_count),
        Stmt::Expr { e, .. } => 1 + expr_node_count(e),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            1 + expr_node_count(cond)
                + stmt_node_count(then_)
                + else_.as_ref().map_or(0, |e| stmt_node_count(e))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            1 + expr_node_count(cond) + stmt_node_count(body)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            1 + init.as_ref().map_or(0, |s| stmt_node_count(s))
                + cond.as_ref().map_or(0, expr_node_count)
                + step.as_ref().map_or(0, expr_node_count)
                + stmt_node_count(body)
        },
        Stmt::Block { b, .. } => 1 + b.items.iter().map(item_node_count).sum::<usize>(),
        Stmt::Switch { expr, cases, .. } => {
            1 + expr_node_count(expr)
                + cases
                    .iter()
                    .map(|c| c.body.items.iter().map(item_node_count).sum::<usize>())
                    .sum::<usize>()
        },
        _ => 1,
    }
}

fn init_node_count(init: &crate::ast::InitItem) -> usize {
    match init {
        crate::ast::InitItem::Expr(e) => expr_node_count(e),
        crate::ast::InitItem::Designated { value, .. } => expr_node_count(value),
    }
}

/// Estimate the cost of inlining function `f` called with `args`.
///
/// Returns the estimated node count of the inlined result.
/// Lower is better; values above `INLINE_COST_THRESHOLD` indicate
/// the inlining would likely increase code size.
fn estimate_inline_cost(_f: &FuncDecl, body: &Block, args: &[Expr]) -> usize {
    // Base cost: all nodes in the function body
    let body_cost: usize = body.items.iter().map(item_node_count).sum();
    // Argument duplication cost: each argument is substituted for every
    // reference to the corresponding parameter in the body.
    // We approximate by counting arg nodes * param references (bounded
    // by body_cost as an upper heuristic).
    let arg_cost: usize = args.iter().map(expr_node_count).sum::<usize>();
    // Total: body + arg duplication (arg cost scaled by param usage ratio)
    body_cost.saturating_add(arg_cost)
}

fn inline_calls_in_block(
    body: &mut Block,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
) {
    for item in &mut body.items {
        match item {
            BlockItem::Stmt(s) => inline_calls_in_stmt(
                s,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            ),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    inline_calls_in_block(
                        b,
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth,
                    );
                }
            },
            _ => {},
        }
    }
}

fn inline_calls_in_stmt(
    s: &mut Stmt,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
) {
    // always_inline / flatten 且函数体含局部声明: 语句级展开.
    // 成功后调用点已被替换为展开后的块, 无需再做表达式级内联.
    if try_stmt_inline_call(s, funcs, caller_flatten, depth) {
        return;
    }
    // P13 阶段5：-O2 起的小 `static` 函数体（带控制流）成本模型内联。展开后立刻
    // 递归走进新块（实参里可能还有可调用的直接调用），深度 +1 兜底。
    if try_static_inline_general(s, funcs, level, depth) {
        inline_calls_in_stmt(
            s,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth + 1,
        );
        return;
    }
    match s {
        Stmt::Return { val: Some(e), .. } => {
            *e = inline_expr(
                e.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
        },
        Stmt::Expr { e, .. } => {
            *e = inline_expr(
                e.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = inline_expr(
                cond.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
            inline_calls_in_stmt(
                then_,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
            if let Some(e) = else_ {
                inline_calls_in_stmt(
                    e,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
        },
        Stmt::Block { b, .. } => inline_calls_in_block(
            b,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
        ),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => inline_calls_in_stmt(
            body,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
        ),
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(c) = cond {
                *c = inline_expr(
                    c.clone(),
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
            if let Some(st) = step {
                *st = inline_expr(
                    st.clone(),
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
            if let Some(i) = init {
                inline_calls_in_stmt(
                    i,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                );
            }
            inline_calls_in_stmt(
                body,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
        },
        _ => {},
    }
}

fn extract_inline_return(body: &Block) -> Option<&Expr> {
    match body.items.as_slice() {
        [BlockItem::Stmt(Stmt::Return { val: Some(e), .. })] => Some(e),
        items if !items.is_empty() => {
            if let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = items.last()? {
                // 表达式级内联只能保留返回表达式; 前置项必须是无运行时
                // 副作用的类型定义. 变量声明 (尤其带初始化器) 会被丢弃,
                // 导致返回表达式引用未定义变量或丢失副作用.
                let only_types = items[..items.len() - 1].iter().all(|it| {
                    matches!(
                        it,
                        BlockItem::Decl(Decl::Typedef { .. })
                            | BlockItem::Decl(Decl::StructDef(_))
                            | BlockItem::Decl(Decl::UnionDef(_))
                            | BlockItem::Decl(Decl::EnumDef(_))
                    )
                });
                if only_types {
                    Some(e)
                } else {
                    None
                }
            } else {
                None
            }
        },
        _ => None,
    }
}

/// 语句级 forced 内联的全局唯一计数器 (每次展开生成不同的临时名)
static STMT_INLINE_UID: AtomicU32 = AtomicU32::new(0);

/// ty 是否含运行时长度数组维度 (VLA). VLA 声明的初始化/分配在
/// 内联展开后无法简单跨语句重写, 遇到即拒绝展开.
fn type_has_vla_dim(t: &crate::ast::Type) -> bool {
    match t {
        crate::ast::Type::Array(_, sz) => matches!(sz, ArraySize::Var(_)),
        crate::ast::Type::Ptr(p) => type_has_vla_dim(&p.base),
        crate::ast::Type::Struct(_, fields, _, _) | crate::ast::Type::Union(_, fields, _, _) => {
            fields.iter().any(|f| type_has_vla_dim(&f.ty))
        },
        _ => false,
    }
}

/// 将表达式中的标识符按 map 重命名 (callee 局部/形参 → 展开临时名).
/// 只重命名 callee body 内的名字; 实参表达式不参与重命名.
fn rename_idents_in_expr(e: &mut Expr, map: &HashMap<String, String>) {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(new) = map.get(&name.name) {
                name.name = new.clone();
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } => {
            rename_idents_in_expr(l, map);
            rename_idents_in_expr(r, map);
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => rename_idents_in_expr(x, map),
        Expr::Call { callee, args, .. } => {
            rename_idents_in_expr(callee, map);
            for a in args {
                rename_idents_in_expr(a, map);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            rename_idents_in_expr(cond, map);
            rename_idents_in_expr(then_, map);
            rename_idents_in_expr(else_, map);
        },
        Expr::Index { arr, idx, .. } => {
            rename_idents_in_expr(arr, map);
            rename_idents_in_expr(idx, map);
        },
        Expr::Member { expr, .. } => rename_idents_in_expr(expr, map),
        Expr::Comma { l, r, .. } => {
            rename_idents_in_expr(l, map);
            rename_idents_in_expr(r, map);
        },
        Expr::VaArg { ap, .. } => rename_idents_in_expr(ap, map),
        Expr::Offsetof { indices, .. } => {
            for i in indices {
                rename_idents_in_expr(i, map);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => rename_idents_in_expr(x, map),
                    InitItem::Designated { value, designator } => {
                        if let crate::ast::Designator::Index(idx) = designator {
                            rename_idents_in_expr(idx, map);
                        }
                        rename_idents_in_expr(value, map);
                    },
                }
            }
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for o in outputs {
                rename_idents_in_expr(o, map);
            }
            for i in inputs {
                rename_idents_in_expr(i, map);
            }
        },
        Expr::StmtExpr { block, .. } => {
            rename_idents_in_items(&mut block.items, map);
        },
        // 字面量/sizeof/alignof/&&label: 不含可重命名的运行时标识符
        _ => {},
    }
}

fn rename_idents_in_stmt(s: &mut Stmt, map: &HashMap<String, String>) {
    match s {
        Stmt::Return { val: Some(e), .. } => rename_idents_in_expr(e, map),
        Stmt::Expr { e, .. } => rename_idents_in_expr(e, map),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            rename_idents_in_expr(cond, map);
            rename_idents_in_stmt(then_, map);
            if let Some(el) = else_ {
                rename_idents_in_stmt(el, map);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            rename_idents_in_expr(cond, map);
            rename_idents_in_stmt(body, map);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                rename_idents_in_stmt(i, map);
            }
            if let Some(c) = cond {
                rename_idents_in_expr(c, map);
            }
            if let Some(st) = step {
                rename_idents_in_expr(st, map);
            }
            rename_idents_in_stmt(body, map);
        },
        Stmt::Block { b, .. } => rename_idents_in_items(&mut b.items, map),
        // 标签语句与 switch 必须跟着改名：P13 阶段5 的函数体内联会把带控制流的
        // body 整体搬进 caller，漏一个分支就是「callee 的 `L` 读到 caller 的 `L`」
        //（实测 Lua `ldo.c` 内联后直接段错误）。以前的调用者（always_inline 那条臂）
        // 只接直线性 body，这两臂从未被走到。
        Stmt::Label { stmt, .. } => rename_idents_in_stmt(stmt, map),
        Stmt::Switch { expr, cases, prelude, .. } => {
            rename_idents_in_expr(expr, map);
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    rename_idents_in_stmt(st, map);
                }
            }
            for c in cases {
                if let Some(v) = &mut c.val {
                    rename_idents_in_expr(v, map);
                }
                rename_idents_in_items(&mut c.body.items, map);
            }
        },
        // C++ 零成本异常: 强制内联的改名必须看到受保护调用内部的语句与落地垫槽位,
        // 否则被内联函数里的同名变量会与该区域内的调用者变量混淆。
        Stmt::Invoke { call, .. } => rename_idents_in_stmt(call, map),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            rename_idents_in_expr(object, map);
            rename_idents_in_expr(selector, map);
        },
        _ => {},
    }
}

fn rename_idents_in_items(items: &mut Vec<BlockItem>, map: &HashMap<String, String>) {
    for it in items {
        match it {
            BlockItem::Stmt(s) => rename_idents_in_stmt(s, map),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    rename_idents_in_expr(init, map);
                }
            },
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    rename_idents_in_items(&mut b.items, map);
                }
            },
            _ => {},
        }
    }
}

/// 构造一个空的局部变量声明 (所有属性字段为默认值)
fn plain_var_decl(ty: crate::ast::Type, name: String, init: Option<Expr>, span: &Span) -> VarDecl {
    VarDecl {
        ty,
        name: Ident {
            name,
            span: span.clone(),
        },
        init,
        span: span.clone(),
        is_extern: false,
        is_static: false,
        section: None,
        alignment: None,
        is_weak: false,
        visibility: None,
        is_unused: false,
        register_name: None,
        home: None,
        cleanup_fn: None,
        is_thread_local: false,
        no_sanitize: vec![],
        alias_name: None,
        copy_fn: None,
        is_retain: false,
        is_used_attr: false,
        quals: QualifierFlags::NONE,
        addr_space: AddrSpace::None,
        tls_model: None,
        asm_label: None,
    }
}

/// 语句级 forced 内联展开.
///
/// 处理函数体含局部变量声明的 always_inline / flatten 调用点:
/// 将 `ret = f(args)` / `f(args)` 展开为
///   { 形参临时声明; callee 局部声明 (重命名); 中间表达式语句; 返回语句 }
///
/// 展开为块后, 形参以临时变量传值 (实参只求值一次, 与 GCC 语义一致).
/// 返回 true 表示调用点已被替换.
/// 阶段5（成本模型驱动的 `static` 函数体内联）的节点数上限。
///
/// 与 -O3 那条启发式（零参 + cost ≤ 20）不是一条路：收益最大的形态（Lua
/// `prepCallInfo`、SQLite btree 的 `fillInCell`/`dropCell`/`allocateSpace`）全是
/// 「多形参 + 体内有 if/goto」的小 static 助手，零参限制把它们全隔在外面。
/// 阈值只能由产物说话：本轮的 Lua/SQLite 运行时、产物尺寸与编译时间就是它的裁判
///（取值过程记在报告 §6.9 与 .trae/documents/x86_64-p13-…-plan.md）。定在 200 是
/// 按实测分布选的：`ldo.c`/`lstate.c` 里那批小助手（`checkmode` 26、`unroll` 43、
/// `resetCI` 68、`resume_error` 75、`stack_init` 84、`preinit_thread` 100、
/// `finishCcall` 125、`correctstack` 127）全在 200 以内，而 `moveresults` 210、
/// `init_registry` 162 这类已经不值得整块搬进 caller。
const STATIC_INLINE_COST_THRESHOLD: usize = 200;

/// 只有**一个调用点**的 `static` 函数放宽到的上限。
///
/// 为什么单独放宽：单点内联不复制代码（原本就要为这一次调用发一份体），所以
/// 不存在「膨胀换性能」的取舍，只剩「少一层调用 + caller 能看见 callee 的常量」的
/// 纯收益。实测反证了这一点：按统一阈值 200 内联时 Lua fib 反而慢 2.5%（多调用点
/// 的小函数被复制了好几份，I-cache 代价盖过省下的 call）。
const STATIC_INLINE_SINGLE_SITE_COST_THRESHOLD: usize = 100_000;

/// 内联 pass 的只读上下文：本 TU 的函数表 + 每个函数名作为**直接调用 callee** 出现的次数。
///
/// 为什么打成一张表而不是再加一个参数：`funcs` 已经穿过整条递归（十几个调用点），
/// 多一个参数就是十几处要同步改的地方，漏一处就是一个静默的阈值错位；把「调用点数」
/// 放进同一张表里随 `funcs` 一起流，单点内联判据与函数表永远同步。
pub(crate) struct InlineTable {
    funcs: HashMap<String, FuncDecl>,
    sites: HashMap<String, usize>,
    /// 出现在任一内联汇编文本里的标识符。被 asm 文本点名的 `static` 函数**不能**被展开掉：
    /// 展开后 C 侧再无引用，定义会被丢掉，而 asm 文本里那句 `call leaf` 还在——链接期
    /// 直接 `undefined reference`（实测夹具 `tests/fixtures/opt/asm_text_static_func_symbol.c`）。
    /// asm 文本不参与符号引用分析，所以这里只能按「文本里出现过这个名字」保守处理。
    asm_names: std::collections::HashSet<String>,
}

impl InlineTable {
    /// 字段的**唯一**组装点：结构体以后再加判据时，不会像刚才那样在某个调用点
    /// 漏掉一个字段（漏掉的后果是「该内联的不内联」或反之，都是静默的）。
    fn build(funcs: HashMap<String, FuncDecl>, prog: &Program) -> Self {
        let sites = count_direct_call_sites(&funcs);
        let asm_names = collect_asm_names(prog);
        InlineTable { sites, asm_names, funcs }
    }

    fn get(&self, name: &str) -> Option<&FuncDecl> {
        self.funcs.get(name)
    }

    /// 该函数名在本 TU 里被直接调用的次数（0 表示没有调用点，例如只在初始化列表里出现）。
    fn call_sites(&self, name: &str) -> usize {
        self.sites.get(name).copied().unwrap_or(0)
    }

    fn named_by_asm(&self, name: &str) -> bool {
        self.asm_names.contains(name)
    }
}

/// 抓出所有内联汇编文本里的标识符（只要 `call foo` / `jmp bar` 写得下，名字就算被引用）。
fn collect_asm_names(prog: &Program) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for d in &prog.decls {
        if let Decl::Func(f) = d {
            if let Some(b) = &f.body {
                asm_names_in_block(b, &mut out);
            }
        }
    }
    out
}

fn push_asm_idents(text: &str, out: &mut std::collections::HashSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let d = bytes[i] as char;
                if d.is_ascii_alphanumeric() || d == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            out.insert(text[start..i].to_string());
        } else {
            i += 1;
        }
    }
}

fn asm_names_in_block(b: &Block, out: &mut std::collections::HashSet<String>) {
    for item in &b.items {
        asm_names_in_item(item, out);
    }
}

fn asm_names_in_item(item: &BlockItem, out: &mut std::collections::HashSet<String>) {
    match item {
        BlockItem::Stmt(s) => asm_names_in_stmt(s, out),
        BlockItem::Decl(Decl::Func(f)) => {
            if let Some(bb) = &f.body {
                asm_names_in_block(bb, out);
            }
        },
        _ => {},
    }
}

fn asm_names_in_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::AsmGoto { asm_str, .. } => push_asm_idents(asm_str, out),
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => asm_names_in_expr(e, out),
        Stmt::If { cond, then_, else_, .. } => {
            asm_names_in_expr(cond, out);
            asm_names_in_stmt(then_, out);
            if let Some(x) = else_ {
                asm_names_in_stmt(x, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            asm_names_in_expr(cond, out);
            asm_names_in_stmt(body, out);
        },
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(i) = init {
                asm_names_in_stmt(i, out);
            }
            if let Some(c) = cond {
                asm_names_in_expr(c, out);
            }
            if let Some(st) = step {
                asm_names_in_expr(st, out);
            }
            asm_names_in_stmt(body, out);
        },
        Stmt::Block { b, .. } => asm_names_in_block(b, out),
        Stmt::Label { stmt, .. } => asm_names_in_stmt(stmt, out),
        Stmt::Switch { expr, cases, prelude, .. } => {
            asm_names_in_expr(expr, out);
            for it in prelude {
                asm_names_in_item(it, out);
            }
            for c in cases {
                asm_names_in_block(&c.body, out);
            }
        },
        _ => {},
    }
}

fn asm_names_in_expr(e: &Expr, out: &mut std::collections::HashSet<String>) {
    if let Expr::AsmExpr {
        asm_str,
        outputs,
        inputs,
        ..
    } = e
    {
        push_asm_idents(asm_str, out);
        for x in outputs.iter().chain(inputs.iter()) {
            asm_names_in_expr(x, out);
        }
    }
}

/// 本 TU 里每个函数名作为**直接调用 callee** 出现的次数（单点内联判据用）。
fn count_direct_call_sites(funcs: &HashMap<String, FuncDecl>) -> HashMap<String, usize> {
    let mut n: HashMap<String, usize> = HashMap::new();
    for (_, f) in funcs {
        let Some(b) = &f.body else { continue };
        for callee in collect_direct_calls(b) {
            *n.entry(callee).or_insert(0) += 1;
        }
    }
    n
}

/// 一次遍历同时拿到两件事：体里调了谁（递归/环判定用），以及有没有「根本不能搬」
/// 的形状。拿不准的变体一律**失败关闭**（`blocked = true`）：宁可少一个优化，
/// 也不要让今后新加的 AST 变体默认被当作「可以内联」。子节点走法照
/// `rename_idents_in_expr` 那套，不另开一份「谁是子表达式」的定义。
#[derive(Default)]
struct InlineScan {
    calls: Vec<String>,
    blocked: bool,
}

impl InlineScan {
    fn note_blocked(&mut self) {
        self.blocked = true;
    }
}

fn scan_items(items: &[BlockItem], acc: &mut InlineScan) {
    for item in items {
        match item {
            BlockItem::Stmt(s) => scan_stmt(s, acc),
            BlockItem::Decl(Decl::Func(_)) => {
                // 嵌套函数定义：闭包链绑在外层帧上，搬不动。
                acc.note_blocked();
            },
            BlockItem::Decl(Decl::Var(v)) => {
                if v.is_static
                    || v.is_extern
                    || v.is_thread_local
                    || v.cleanup_fn.is_some()
                    || v.register_name.is_some()
                    || v.alias_name.is_some()
                    || v.asm_label.is_some()
                    || type_has_vla_dim(&v.ty)
                {
                    acc.note_blocked();
                }
                if let Some(e) = &v.init {
                    scan_expr(e, acc);
                }
            },
            // 类型声明无运行时语义，留在展开块里不影响正确性（callee 局部可能用它）。
            BlockItem::Decl(Decl::Typedef { .. })
            | BlockItem::Decl(Decl::StructDef(_))
            | BlockItem::Decl(Decl::UnionDef(_))
            | BlockItem::Decl(Decl::EnumDef(_)) => {},
            _ => acc.note_blocked(),
        }
    }
}

fn scan_block(b: &Block, acc: &mut InlineScan) {
    scan_items(&b.items, acc);
}

fn scan_stmt(s: &Stmt, acc: &mut InlineScan) {
    match s {
        // computed goto / asm goto：标签地址与指令流跟当前函数绑死，不能搬。
        Stmt::GotoIndirect { .. } | Stmt::AsmGoto { .. } => acc.note_blocked(),
        Stmt::Expr { e, .. } => scan_expr(e, acc),
        Stmt::Return { val: Some(e), .. } => scan_expr(e, acc),
        Stmt::Return { val: None, .. } | Stmt::Goto { .. } | Stmt::Break { .. }
        | Stmt::Continue { .. } => {},
        Stmt::If { cond, then_, else_, .. } => {
            scan_expr(cond, acc);
            scan_stmt(then_, acc);
            if let Some(e) = else_ {
                scan_stmt(e, acc);
            }
        },
        Stmt::While { cond, body, .. } => {
            scan_expr(cond, acc);
            scan_stmt(body, acc);
        },
        Stmt::Do { cond, body, .. } => {
            scan_expr(cond, acc);
            scan_stmt(body, acc);
        },
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(i) = init {
                scan_stmt(i, acc);
            }
            if let Some(c) = cond {
                scan_expr(c, acc);
            }
            if let Some(st) = step {
                scan_expr(st, acc);
            }
            scan_stmt(body, acc);
        },
        Stmt::Block { b, .. } => scan_block(b, acc),
        Stmt::Label { stmt, .. } => scan_stmt(stmt, acc),
        Stmt::Switch { expr, cases, prelude, .. } => {
            scan_expr(expr, acc);
            scan_items(prelude, acc);
            for c in cases {
                if let Some(v) = &c.val {
                    scan_expr(v, acc);
                }
                scan_block(&c.body, acc);
            }
        },
        _ => acc.note_blocked(),
    }
}

fn scan_expr(e: &Expr, acc: &mut InlineScan) {
    match e {
        // 这些形态都隐含「帧/标签就在本函数里」，搬进调用方就错。
        Expr::StmtExpr { .. }
        | Expr::LabelAddr { .. }
        | Expr::VaArg { .. }
        | Expr::VaArgPack { .. }
        | Expr::AsmExpr { .. } => acc.note_blocked(),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                acc.calls.push(name.name.clone());
            } else {
                // 函数指针与被计算的 callee：本身也要扫（里面可能有调用）。
                scan_expr(callee, acc);
            }
            for a in args {
                scan_expr(a, acc);
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            scan_expr(l, acc);
            scan_expr(r, acc);
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => scan_expr(x, acc),
        Expr::Cond { cond, then_, else_, .. } => {
            scan_expr(cond, acc);
            scan_expr(then_, acc);
            scan_expr(else_, acc);
        },
        Expr::Index { arr, idx, .. } => {
            scan_expr(arr, acc);
            scan_expr(idx, acc);
        },
        Expr::Member { expr, .. } => scan_expr(expr, acc),
        // 初始化列表/复合字面量：带 designator 的整块初始化今天不搬（失败关闭）。
        Expr::CompoundLit { .. } | Expr::InitList { .. } => acc.note_blocked(),
        // `offsetof(T, a[i])` 的下标表达式在编译期不参与求值，但可能有副作用，
        // 而 designator 里的表达式也不影响「调了谁」的结论，扫一下不算漏。
        Expr::Offsetof { indices, .. } => {
            for i in indices {
                scan_expr(i, acc);
            }
        },
        // 叶子：字面量/名字/`sizeof(T)`/`_Alignof(T)`。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. } => {},
        _ => acc.note_blocked(),
    }
}

/// 收集一个函数体里出现的**直接调用**名字（用于递归/环判定）。
fn collect_direct_calls(body: &Block) -> Vec<String> {
    let mut acc = InlineScan::default();
    scan_block(body, &mut acc);
    acc.calls
}

/// `funcs[from]` 的调用闭包能否回到 `target`（间接递归也算）。
fn calls_graph_reaches(
    funcs: &InlineTable,
    from: &str,
    target: &str,
) -> bool {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut work: Vec<String> = vec![from.to_string()];
    while let Some(name) = work.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let f = match funcs.get(&name) {
            Some(f) => f,
            None => continue,
        };
        let body = match &f.body {
            Some(b) => b,
            None => continue,
        };
        for callee in collect_direct_calls(body) {
            if callee == target {
                return true;
            }
            if !seen.contains(&callee) {
                work.push(callee);
            }
        }
    }
    false
}

/// 可以被**整体搬进调用方**的函数体判据：控制流可以，但不能出现任何
/// 「跨帧/跨指令流」形状。每一条都是硬拒绝，不做「看起来安全就试一下」：
///   * `goto *ptr` / `Stmt::AsmGoto` / `Expr::LabelAddr`：标签地址进了 callee 就
///     与 caller 的标签集混在一起，而且 computed goto 本身也不允许搬；
///   * `Expr::StmtExpr`：语句表达式里可以藏 `return`，那个 `return` 属于 callee，
///     改写器不递进表达式 → 会变成从 caller 里 return；
///   * `Expr::VaArg` / `VaArgPack` / `AsmExpr` / 任何 `__builtin_*` 调用：变参寄存器
///     保存区、`__builtin_return_address` 这类都隐含「帧就在脚下」；
///   * 嵌套函数定义、`static`/`_Thread_local`/`extern`/cleanup/register/asm 标签变量、VLA：
///     存储期或帧布局不搬得动；
///   * 调用 KNOWN_RETURNS_TWICE 名字族（setjmp 那些）：callee 内部有二次返回点，
///     展开后汇合点会跳到 caller 里。
fn body_is_splicable(body: &Block) -> bool {
    let mut acc = InlineScan::default();
    scan_block(body, &mut acc);
    if acc.blocked {
        return false;
    }
    for name in &acc.calls {
        let n = name.as_str();
        if n.starts_with("__builtin") || KNOWN_RETURNS_TWICE.contains(&n) {
            return false;
        }
    }
    true
}

/// 把 callee 体里的标签与 `goto` 目标统一改名（一次展开一份，避免与 caller 同名的
/// 标签相撞；C 的标签作用域在函数内，所以不重命名会直接编译成错的跳转）。
fn prefix_labels_in_stmt(s: &mut Stmt, prefix: &str) {
    match s {
        Stmt::Label { name, stmt, .. } => {
            name.name = format!("{}{}", prefix, name.name);
            prefix_labels_in_stmt(stmt, prefix);
        },
        Stmt::Goto { target, .. } => {
            target.name = format!("{}{}", prefix, target.name);
        },
        Stmt::If { then_, else_, .. } => {
            prefix_labels_in_stmt(then_, prefix);
            if let Some(e) = else_ {
                prefix_labels_in_stmt(e, prefix);
            }
        },
        Stmt::While { body, .. } => prefix_labels_in_stmt(body, prefix),
        Stmt::Do { body, .. } => prefix_labels_in_stmt(body, prefix),
        Stmt::Block { b, .. } => {
            for it in &mut b.items {
                if let BlockItem::Stmt(st) = it {
                    prefix_labels_in_stmt(st, prefix);
                }
            }
        },
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prefix_labels_in_stmt(i, prefix);
            }
            prefix_labels_in_stmt(body, prefix);
        },
        Stmt::Switch { cases, prelude, .. } => {
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    prefix_labels_in_stmt(st, prefix);
                }
            }
            for c in cases {
                for bit in &mut c.body.items {
                    if let BlockItem::Stmt(st) = bit {
                        prefix_labels_in_stmt(st, prefix);
                    }
                }
            }
        },
        _ => {},
    }
}

/// 把 callee 体里的 `Stmt::Block` 展开一遍标签（`prefix_labels_in_stmt` 对 Block 的
/// 专用分支）。
fn prefix_labels_in_items(items: &mut Vec<BlockItem>, prefix: &str) {
    for it in items {
        if let BlockItem::Stmt(st) = it {
            prefix_labels_in_stmt(st, prefix);
        }
    }
}

/// 调用点值被丢弃时改写 `return`：`return e;` → `{ e; goto end; }`，`return;` → `goto end;`。
/// 递归进所有控制流子树（callee 的 `return` 可以在任意嵌套层级上）。
fn rewrite_returns_discarded(s: &mut Stmt, end_label: &str, span: &Span) {
    match s {
        Stmt::Return { val, .. } => {
            let val = val.take();
            let goto = Stmt::Goto {
                target: Ident {
                    name: end_label.to_string(),
                    span: span.clone(),
                },
                span: span.clone(),
            };
            *s = match val {
                Some(e) => Stmt::Block {
                    b: Block {
                        items: vec![
                            BlockItem::Stmt(Stmt::Expr { e, span: span.clone() }),
                            BlockItem::Stmt(goto),
                        ],
                        span: span.clone(),
                    },
                    span: span.clone(),
                },
                None => goto,
            };
        },
        Stmt::If { then_, else_, .. } => {
            rewrite_returns_discarded(then_, end_label, span);
            if let Some(e) = else_ {
                rewrite_returns_discarded(e, end_label, span);
            }
        },
        Stmt::While { body, .. } => rewrite_returns_discarded(body, end_label, span),
        Stmt::Do { body, .. } => rewrite_returns_discarded(body, end_label, span),
        Stmt::Block { b, .. } => {
            for it in &mut b.items {
                if let BlockItem::Stmt(st) = it {
                    rewrite_returns_discarded(st, end_label, span);
                }
            }
        },
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                rewrite_returns_discarded(i, end_label, span);
            }
            rewrite_returns_discarded(body, end_label, span);
        },
        Stmt::Label { stmt, .. } => rewrite_returns_discarded(stmt, end_label, span),
        Stmt::Switch { cases, prelude, .. } => {
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    rewrite_returns_discarded(st, end_label, span);
                }
            }
            for c in cases {
                for bit in &mut c.body.items {
                    if let BlockItem::Stmt(st) = bit {
                        rewrite_returns_discarded(st, end_label, span);
                    }
                }
            }
        },
        _ => {},
    }
}

/// 把 callee 体里**所有层**的局部声明改成展开后的唯一名字，并把 old→new 记进 `map`。
///
/// 必须递归到嵌套块/switch case/if 体内：`rename_idents_in_items` 只改表达式不改声明名，
/// 而本 pass 跑在语义分析之后，漏改的引用会到 codegen 才被懒注册成另一个栈槽（静默错值）。
/// 只改 `Decl::Var`：callee 的 `static`/`extern` 变量已在 `InlineScan` 里被拒绝，
/// 类型声明与函数声明不改名。
fn rename_decl_names_in_item(item: &mut BlockItem, uid: u32, map: &mut HashMap<String, String>) {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            let new = format!("__inl{}_l{}", uid, v.name.name);
            map.insert(v.name.name.clone(), new.clone());
            v.name.name = new;
            if let Some(e) = &mut v.init {
                rename_decl_names_in_expr(e, uid, map);
            }
        },
        BlockItem::Stmt(s) => rename_decl_names_in_stmt(s, uid, map),
        _ => {},
    }
}

fn rename_decl_names_in_stmt(s: &mut Stmt, uid: u32, map: &mut HashMap<String, String>) {
    match s {
        Stmt::Block { b, .. } => {
            for it in &mut b.items {
                rename_decl_names_in_item(it, uid, map);
            }
        },
        Stmt::If { then_, else_, .. } => {
            rename_decl_names_in_stmt(then_, uid, map);
            if let Some(e) = else_ {
                rename_decl_names_in_stmt(e, uid, map);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::Label { stmt: body, .. } => {
            rename_decl_names_in_stmt(body, uid, map);
        },
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                rename_decl_names_in_stmt(i, uid, map);
            }
            rename_decl_names_in_stmt(body, uid, map);
        },
        Stmt::Switch { cases, prelude, .. } => {
            for it in prelude {
                rename_decl_names_in_item(it, uid, map);
            }
            for c in cases {
                for it in &mut c.body.items {
                    rename_decl_names_in_item(it, uid, map);
                }
            }
        },
        _ => {},
    }
}

/// 声明初始化器里可能嵌着语句表达式/复合字面量；它们的子作用域今天都被
/// `InlineScan` 拒了（复合字面量/语句表达式在名单外），所以这里只递回表达式自身。
fn rename_decl_names_in_expr(_e: &mut Expr, _uid: u32, _map: &mut HashMap<String, String>) {}

/// P13 阶段5：-O2 起对**小 `static` 函数**做函数体级内联（允许控制流）。
///
/// 为什么单独一条臂而不是改 `always_inline` 那条：那条的准入是「程序员明说」，
/// 本条是「成本模型推断」，两者错了的代价完全不同（前者是用户表达错了，后者是
/// 编译器猜错了），判据不能合在一起改。也只接两个调用点形状：值被丢弃的表达式
/// 语句与尾位置 `return f(...)`；值还要参与更大表达式的情形需要语句表达式包装，
/// 而包装后的 `return`/`goto` 语义还要另讲，不在这里探一下。
fn try_static_inline_general(
    s: &mut Stmt,
    funcs: &InlineTable,
    level: u8,
    depth: u32,
) -> bool {
    if level < 2 || depth >= INLINE_MAX_DEPTH {
        return false;
    }
    let (callee_name, args, in_return) = match s {
        Stmt::Expr { e: Expr::Call { callee, args, .. }, span: _ } => match callee.as_ref() {
            Expr::Ident { name, .. } => (name.name.clone(), args.clone(), false),
            _ => return false,
        },
        Stmt::Return { val: Some(Expr::Call { callee, args, .. }), span: _ } => {
            match callee.as_ref() {
                Expr::Ident { name, .. } => (name.name.clone(), args.clone(), true),
                _ => return false,
            }
        },
        _ => return false,
    };
    let f = match funcs.get(&callee_name) {
        Some(f) => f,
        None => return false,
    };
    // 准入：内部链接、没被明说禁止、不是那些已有专属路径的形状。
    if !f.is_static
        || f.is_noinline
        || f.is_always_inline
        || f.is_variadic
        || f.returns_twice
        || f.is_nested
        || f.is_old_style
        || f.alias_name.is_some()
        // 被内联汇编文本点名的 `static`：展开掉就没有人再引用它，定义会消失，
        // 而 asm 文本里的 `call leaf` 还在 ⇒ 链接期 undefined reference。
        || funcs.named_by_asm(&callee_name)
    {
        return false;
    }
    if f.params.len() != args.len() {
        return false;
    }
    for p in &f.params {
        if p.name.is_none() || p.vla_dim.is_some() || type_has_vla_dim(&p.ty) {
            return false;
        }
    }
    let body = match &f.body {
        Some(b) => b,
        None => return false,
    };
    if !body_is_splicable(body) {
        return false;
    }
    // 递归（包括间接递归）一律不展开：不展开不会错，展开会爆栈。
    if calls_graph_reaches(funcs, &callee_name, &callee_name) {
        return false;
    }
    let cost = estimate_inline_cost(f, body, &args);
    // 多调用点的函数按 200 卡住；只有一个调用点的放宽（不复制代码）。
    let sites = funcs.call_sites(&callee_name);
    let limit = if sites <= 1 {
        STATIC_INLINE_SINGLE_SITE_COST_THRESHOLD
    } else {
        STATIC_INLINE_COST_THRESHOLD
    };
    if cost > limit {
        trace_inline_decision(
            &callee_name,
            "REJECT",
            &format!("static-inline cost {} > {} ({} 个调用点)", cost, limit, sites),
        );
        return false;
    }
    let rspan: Span = match s {
        Stmt::Expr { span, .. } | Stmt::Return { span, .. } => span.clone(),
        _ => return false,
    };

    let uid = STMT_INLINE_UID.fetch_add(1, Ordering::Relaxed);
    let label_prefix = format!("__inl{}__l", uid);
    let end_label = format!("__inl{}__end", uid);
    let mut map: HashMap<String, String> = HashMap::new();
    let mut bind: Vec<BlockItem> = Vec::new();
    // 形参 → 临时量。不用「把实参表达式直接代入引用处」是因为参数可写（形参是
    // 局部变量），代入会改变求值次数与写入目标；先存后改与 callee 语义逐字一致。
    //
    // **实参表达式属于 caller 作用域，绝不能跟着改名**：否则 `f_luaopen(L)` 里
    // `stack_init(L)` 的实参 `L` 会被改成 `__inlN$_L`（那是刚要新建的形参临时量本身）。
    // 旧代码里 `bind` 与 callee 自己的声明混在同一个 `items` 里一起改名，同一个坑。
    for (p, a) in f.params.iter().zip(args.iter()) {
        let pname = match p.name.as_ref() {
            Some(id) => id.name.clone(),
            None => return false,
        };
        let tmp = format!("__inl{}$_{}", uid, pname);
        map.insert(pname, tmp.clone());
        bind.push(BlockItem::Decl(Decl::Var(plain_var_decl(
            p.ty.clone(),
            tmp,
            Some(a.clone()),
            &p.span,
        ))));
    }
    // callee 自己的声明与语句整体搬进来（这些才在 callee 作用域里，需要改名）。
    let mut moved: Vec<BlockItem> = body.items.clone();
    // **先把 callee 局部声明本身改完名**再改引用：`rename_idents_in_items` 只改表达式，
    // 不改 `Decl::Var` 的名字字段。漏了这一步就是「声明叫 `L`、引用叫 `__inlN_lL`」，
    // 而本 pass 跑在语义分析之后，未解析的名字不会被报错，而是到了 codegen 才
    // 懒注册成一个新栈帧槽——实测 Lua 内联后直接段错误。
    for it in moved.iter_mut() {
        rename_decl_names_in_item(it, uid, &mut map);
    }
    if !in_return {
        // 值被丢弃：每个 `return` 先求值再跳到展开块的出口。
        let mut rewritten: Vec<BlockItem> = Vec::new();
        for it in moved.drain(..) {
            match it {
                BlockItem::Stmt(mut st) => {
                    rewrite_returns_discarded(&mut st, &end_label, &rspan);
                    rewritten.push(BlockItem::Stmt(st));
                },
                other => rewritten.push(other),
            }
        }
        moved = rewritten;
        moved.push(BlockItem::Stmt(Stmt::Label {
            name: Ident {
                name: end_label.clone(),
                span: rspan.clone(),
            },
            stmt: Box::new(Stmt::Block {
                b: Block { items: vec![], span: rspan.clone() },
                span: rspan.clone(),
            }),
            span: rspan.clone(),
        }));
    }
    prefix_labels_in_items(&mut moved, &label_prefix);
    rename_idents_in_items(&mut moved, &map);
    let mut items: Vec<BlockItem> = bind;
    items.extend(moved);
    *s = Stmt::Block {
        b: Block { items, span: rspan.clone() },
        span: rspan.clone(),
    };
    trace_inline_decision(
        &callee_name,
        "INLINE",
        &format!("static body inline (cost {}, depth {})", cost, depth),
    );
    true
}

fn try_stmt_inline_call(
    s: &mut Stmt,
    funcs: &InlineTable,
    caller_flatten: bool,
    depth: u32,
) -> bool {
    // 提取调用点 (return f(...) 或表达式语句 f(...))
    let (callee_name, args, in_return) = match s {
        Stmt::Expr {
            e: Expr::Call { callee, args, .. },
            ..
        } => match callee.as_ref() {
            Expr::Ident { name, .. } => (name.name.clone(), args.clone(), false),
            _ => return false,
        },
        Stmt::Return {
            val: Some(Expr::Call { callee, args, .. }),
            ..
        } => match callee.as_ref() {
            Expr::Ident { name, .. } => (name.name.clone(), args.clone(), true),
            _ => return false,
        },
        _ => return false,
    };
    let f = match funcs.get(&callee_name) {
        Some(f) => f,
        None => return false,
    };
    // returns_twice 函数 (setjmp 族/vfork/getcontext) 绝不能内联: 内联会把
    // 「二次返回」点复制进调用方, 破坏调用方的栈/寄存器恢复语义。即使
    // always_inline/flatten 也拒绝 (与 GCC 行为一致)。
    if f.returns_twice {
        trace_inline_decision(&callee_name, "REJECT", "returns_twice");
        return false;
    }
    if !(f.is_always_inline || caller_flatten) {
        return false;
    }
    if f.is_variadic || args.len() != f.params.len() {
        return false;
    }
    if depth >= INLINE_MAX_DEPTH {
        trace_inline_decision(
            &callee_name,
            "REJECT",
            "stmt-inline depth limit (possible mutual recursion)",
        );
        return false;
    }
    let body = match f.body.as_ref() {
        Some(b) => b,
        None => return false,
    };
    // 单 return 表达式体交给表达式级替换 (避免生成不必要的临时变量)
    if matches!(body.items.as_slice(), [BlockItem::Stmt(Stmt::Return { .. })]) {
        return false;
    }

    // 结构检查: 只支持 [VarDecl]* [ExprStmt]* [Return?] 形式的函数体.
    // 控制流 (if/loop/goto/嵌套块/switch) 与特殊存储类别 (static/extern/
    // tls/register/cleanup/VLA) 保留普通调用, 避免错误重写.
    let mut decls: Vec<VarDecl> = Vec::new();
    let mut mid: Vec<Stmt> = Vec::new();
    let mut ret_val: Option<Option<Expr>> = None;
    for (i, item) in body.items.iter().enumerate() {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                if v.is_static
                    || v.is_extern
                    || v.is_thread_local
                    || v.cleanup_fn.is_some()
                    || v.register_name.is_some()
                    || v.alias_name.is_some()
                    || v.asm_label.is_some()
                    || type_has_vla_dim(&v.ty)
                {
                    return false;
                }
                decls.push(v.clone());
            },
            // 类型定义无运行时副作用, 保留在展开块中
            BlockItem::Decl(Decl::Typedef { .. })
            | BlockItem::Decl(Decl::StructDef(_))
            | BlockItem::Decl(Decl::UnionDef(_))
            | BlockItem::Decl(Decl::EnumDef(_)) => {},
            BlockItem::Stmt(st @ Stmt::Expr { .. }) => mid.push(st.clone()),
            BlockItem::Stmt(Stmt::Return { val, .. }) if i + 1 == body.items.len() => {
                ret_val = Some(val.clone());
            },
            // 非 return 结尾/中间 return/其他语句形态: 不展开
            _ => return false,
        }
    }
    // void 返回且调用点是表达式语句: 保留普通调用即可 (无副作用丢失风险)
    let ret_val = match ret_val {
        Some(v) => v,
        None => {
            trace_inline_decision(&callee_name, "REJECT", "stmt-inline: no trailing return");
            return false;
        },
    };
    let rspan = match s {
        Stmt::Return { span, .. } | Stmt::Expr { span, .. } => span.clone(),
        _ => unreachable!(),
    }
    .clone();

    // 名字重映射: 形参与局部变量 → 展开临时名 (前缀 + uid, 避免与
    // 调用方同名变量及多次展开互相冲突)
    let uid = STMT_INLINE_UID.fetch_add(1, Ordering::Relaxed);
    let mut map: HashMap<String, String> = HashMap::new();
    // `bind` 与 `items` 必须分开：前者是「把 caller 的实参存进形参临时量」，表达式属于
    // **caller 作用域**，不能跟着 callee 的名字重映射走；后者才是 callee 自己的声明与语句。
    // 旧写法把两者塞进同一个 `items` 再整体改名（实参会被改成刚建的形参名），而且
    // `mid`（callee 的中间语句）是在改名**之后**才 push 的，等于从来没被改过名——
    // 两个错正好叠在一起，只在「caller 有同名变量」时露出来。
    let mut bind: Vec<BlockItem> = Vec::new();
    let mut items: Vec<BlockItem> = Vec::new();
    for (p, a) in f.params.iter().zip(args.iter()) {
        let pname = match p.name.as_ref() {
            Some(id) => &id.name,
            None => return false,
        };
        let tmp = format!("__inl{}_${}", uid, pname);
        map.insert(pname.clone(), tmp.clone());
        bind.push(BlockItem::Decl(Decl::Var(plain_var_decl(
            p.ty.clone(),
            tmp,
            Some(a.clone()),
            &p.name
                .as_ref()
                .map(|i| i.span.clone())
                .unwrap_or_else(|| rspan.clone()),
        ))));
    }
    for mut v in decls {
        let new = format!("__inl{}_l{}", uid, v.name.name);
        map.insert(v.name.name.clone(), new.clone());
        v.name.name = new;
        items.push(BlockItem::Decl(Decl::Var(v)));
    }
    for st in mid {
        items.push(BlockItem::Stmt(st));
    }
    // 展开块内的 callee 名字重命名（声明初始化器与中间语句一起）
    rename_idents_in_items(&mut items, &map);
    // 返回语句 (ret_val 的表达式已随 items 之前的 rename 处理? 不:
    // ret_val 尚未插入 items, 单独重命名)
    match ret_val {
        Some(e) => {
            let mut e = e;
            rename_idents_in_expr(&mut e, &map);
            items.push(BlockItem::Stmt(if in_return {
                Stmt::Return {
                    val: Some(e),
                    span: rspan.clone(),
                }
            } else {
                Stmt::Expr {
                    e,
                    span: rspan.clone(),
                }
            }));
        },
        None => {
            if in_return {
                items.push(BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: rspan.clone(),
                }));
            }
            // void 返回且调用点是表达式语句: 无需返回语句
        },
    }
    trace_inline_decision(&callee_name, "INLINE", "always_inline (statement expansion)");
    // 形参绑定必须排在最前（callee 的声明/语句会读它们），而它们又不参与改名，
    // 所以只能到最后一步才拼回去。
    items.splice(0..0, bind);
    *s = Stmt::Block {
        b: Block {
            items,
            span: rspan.clone(),
        },
        span: rspan,
    };
    // 展开块内仍可能有嵌套调用 (如 callee 调用另一个 always_inline)
    if let Stmt::Block { b, .. } = s {
        inline_calls_in_block(b, funcs, 0, caller_flatten, false, false, depth + 1);
    }
    true
}

fn expr_calls_name(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Call { callee, args, .. } => {
            let direct = matches!(callee.as_ref(), Expr::Ident { name: n, .. } if n.name == name);
            direct || args.iter().any(|a| expr_calls_name(a, name))
        },
        Expr::Binop { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        Expr::Unary { e, .. } => expr_calls_name(e, name),
        Expr::Cast { e, .. } => expr_calls_name(e, name),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_calls_name(cond, name)
                || expr_calls_name(then_, name)
                || expr_calls_name(else_, name)
        },
        Expr::Comma { l, r, .. } => expr_calls_name(l, name) || expr_calls_name(r, name),
        _ => false,
    }
}

/// 形参绑定替换 (内联展开的核心)。`pack_args` 非空时, 表达式里出现的
/// `Expr::VaArgPack` 展开为这串实参 (原地 splicing 进所在 Call 的实参表)
/// —— `__builtin_va_arg_pack ()` 的内联语义, 见 `try_inline_variadic_pack`。
fn substitute_params(expr: Expr, params: &[(String, Expr)], pack_args: &[Expr]) -> Expr {
    match expr {
        Expr::Ident { name, span, ty } => {
            if let Some((_, arg)) = params.iter().find(|(n, _)| n == &name.name) {
                arg.clone()
            } else {
                Expr::Ident { name, span, ty }
            }
        },
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(substitute_params(*l, params, pack_args)),
            r: Box::new(substitute_params(*r, params, pack_args)),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(substitute_params(*e, params, pack_args)),
            span,
            ty,
        },
        Expr::Cast { ty, e, span } => Expr::Cast {
            ty,
            e: Box::new(substitute_params(*e, params, pack_args)),
            span,
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            span,
            ty,
        } => Expr::Cond {
            cond: Box::new(substitute_params(*cond, params, pack_args)),
            then_: Box::new(substitute_params(*then_, params, pack_args)),
            else_: Box::new(substitute_params(*else_, params, pack_args)),
            span,
            ty,
        },
        Expr::Comma { l, r, span, ty } => Expr::Comma {
            l: Box::new(substitute_params(*l, params, pack_args)),
            r: Box::new(substitute_params(*r, params, pack_args)),
            span,
            ty,
        },
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => Expr::Call {
            callee: Box::new(substitute_params(*callee, params, pack_args)),
            // `f(a, __builtin_va_arg_pack())` → `f(a, pack0, pack1, ...)`:
            // pack 占一个实参位, 展开成调用方多余实参的**整串**。
            args: args
                .into_iter()
                .flat_map(|a| match a {
                    Expr::VaArgPack { .. } if !pack_args.is_empty() => {
                        pack_args.iter().cloned().map(Some).collect::<Vec<_>>()
                    },
                    other => vec![Some(substitute_params(other, params, pack_args))],
                })
                .flatten()
                .collect(),
            span,
            ty,
        },
        other => other,
    }
}

/// 内联决策: 判断函数 `name` 是否可被内联, 并返回替换后的表达式.
///
/// 决策规则 (按优先级):
///   1. `noinline` 且非 `always_inline`: 拒绝 (noinline 被尊重)
///   2. `always_inline` 与 `noinline` 同时存在: `always_inline` 优先 (GCC 兼容)
///   3. `always_inline` / `flatten`: 无条件接受 (不受尺寸/深度限制)
///   4. O0 级别: 拒绝所有非强制内联
///   5. 深度超限: 拒绝
///   6. 直接递归: 拒绝 (callee 名与函数名相同)
///   7. Profile-guided (hot caller, O2+): 放宽阈值 (cost ≤ 40, 不要求 static)
///   8. O3 启发式: static + 零参 + cost ≤ INLINE_COST_THRESHOLD (20)
///   9. 其他: 拒绝
///
/// 当 DCC_INLINE_TRACE=1 时, 每个决策均输出到 stderr.
fn try_inline_call(
    name: &str,
    args: &[Expr],
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    depth: u32,
) -> Option<Expr> {
    let f = funcs.get(name)?;
    // returns_twice 函数 (setjmp 族/vfork/getcontext) 绝不能内联: 内联会把
    // 「二次返回」点复制进调用方, 破坏调用方的栈/寄存器恢复语义。即使
    // always_inline/flatten 也拒绝 (与 GCC 行为一致)。
    if f.returns_twice {
        trace_inline_decision(name, "REJECT", "returns_twice");
        return None;
    }
    let body = f.body.as_ref()?;

    // --- 变参 + `__builtin_va_arg_pack ()` 的强制内联 (先行判定) ---
    // GCC 语义: 该内建只在函数被内联后有意义 —— 展开为「把调用方的多余
    // 实参原地塞回 pack 出现处」。非内联时 GCC 直接报错, dcc 则落到
    // codegen 的寄存器保存区镜像转发 (只能覆盖"全寄存器在前"的布局,
    // va-arg-pack-1 的 long double 在前形状会错位)。这里只放行
    // always_inline/flatten 且实参数 ≥ 形参数的调用, 其余变参形状仍走
    // 下面的统一拒绝, 不改变既有行为。
    if f.is_variadic && (f.is_always_inline || caller_flatten) && args.len() >= f.params.len() {
        if let Some(inlined) = try_inline_variadic_pack(f, args, name) {
            return Some(inlined);
        }
    }
    if f.is_variadic || args.len() != f.params.len() {
        trace_inline_decision(name, "SKIP", "variadic/arity mismatch");
        return None;
    }

    // --- 属性优先级: noinline vs always_inline ---
    // GCC 行为: 两者同时标记时 always_inline 优先.
    if f.is_noinline && !f.is_always_inline {
        trace_inline_decision(name, "REJECT", "noinline attribute");
        return None;
    }

    // --- 强制内联: always_inline / flatten ---
    // 不受尺寸限制, 不受深度限制. 内核 __always_inline 依赖此行为.
    // 表达式级替换仅支持单 return 语句的函数体; 含局部变量声明等
    // 结构的 always_inline 函数体由 try_stmt_inline_call 做语句级展开,
    // 此处拒绝内联 (保留普通调用) 以避免丢失声明副作用.
    let forced = f.is_always_inline || caller_flatten;
    if forced {
        if let [BlockItem::Stmt(Stmt::Return { val: Some(ret), .. })] = body.items.as_slice() {
            if expr_calls_name(ret, name) {
                trace_inline_decision(name, "REJECT", "forced but direct recursion");
                return None; // 直接递归保护
            }
            trace_inline_decision(name, "INLINE", "always_inline (expression body)");
            return Some(substitute_call(f, ret, args));
        }
        trace_inline_decision(
            name,
            "REJECT",
            "forced non-expression body (stmt-level inline handled at call site)",
        );
        return None;
    }

    // --- O0: 非强制内联一律拒绝 ---
    if level == 0 {
        trace_inline_decision(name, "REJECT", "O0 non-forced");
        return None;
    }

    // --- 深度限制: 防止嵌套内联爆炸 ---
    if depth >= INLINE_MAX_DEPTH {
        trace_inline_decision(name, "REJECT", &format!("depth {} >= {}", depth, INLINE_MAX_DEPTH));
        return None;
    }

    // --- O3 启发式: 小型 static 零参函数, 使用成本模型 ---
    // Profile-guided: hot caller 在 O2+ 享受放宽阈值:
    //   - 不要求 static
    //   - 允许有参数
    //   - 成本阈值从 20 提升到 40
    let cost = estimate_inline_cost(f, body, args);
    let profile_relaxed = caller_is_hot && level >= 2;
    let heuristic_ok = if profile_relaxed {
        cost <= INLINE_COST_THRESHOLD * 2
    } else {
        f.is_static && f.params.is_empty() && level >= 3 && cost <= INLINE_COST_THRESHOLD
    };
    if !heuristic_ok {
        trace_inline_decision(
            name,
            "REJECT",
            &format!(
                "cost {} > {} or not O3-static{}",
                cost,
                INLINE_COST_THRESHOLD,
                if profile_relaxed { " (profile-relaxed)" } else { "" },
            ),
        );
        return None;
    }

    let ret = extract_inline_return(body)?;
    if expr_calls_name(ret, name) {
        trace_inline_decision(name, "REJECT", "O3 heuristic but direct recursion");
        return None; // 直接递归保护
    }
    trace_inline_decision(
        name,
        "INLINE",
        &format!(
            "O3 heuristic, cost={}{}",
            cost,
            if profile_relaxed { " (profile-relaxed)" } else { "" },
        ),
    );
    Some(substitute_call(f, ret, args))
}

/// 参数替换: 将函数体返回表达式中的形参替换为实参.
fn substitute_call(f: &FuncDecl, ret: &Expr, args: &[Expr]) -> Expr {
    let bindings: Vec<(String, Expr)> = f
        .params
        .iter()
        .zip(args.iter())
        .filter_map(|(p, a)| p.name.as_ref().map(|id| (id.name.clone(), a.clone())))
        .collect();
    if bindings.len() != f.params.len() {
        return ret.clone();
    }
    substitute_params(ret.clone(), &bindings, &[])
}

/// 变参 + `__builtin_va_arg_pack ()` 的强制内联 (va-arg-pack-1)。
///
/// 放行条件 (全部满足才展开, 否则返回 None 保留普通调用):
///   1. 被调方是变参函数且带 `always_inline` (或调用方 flatten);
///   2. 实参数 ≥ 形参数 (多出的就是要转发的变参);
///   3. 函数体形状是 `[Return(v)]` 或
///      `[If{c, then: Return(a), else: None}, Return(b)]`
///      (后者等价 `return c ? a : b;`, GCC 的 va_arg_pack 转发函数正是它);
///   4. 返回值表达式里确实出现 `Expr::VaArgPack` —— 否则多余实参会被
///      静默丢弃, 必须拒绝。
///
/// 展开: 形参 ← 前 n 个实参; `VaArgPack` ← 剩余实参列表 (原地 splicing 进
/// 所在 Call 的实参表, 与 GCC「pack 展开为调用方实参」同源)。
fn try_inline_variadic_pack(f: &FuncDecl, args: &[Expr], name: &str) -> Option<Expr> {
    let body = f.body.as_ref()?;
    let ret = inline_body_return_expr(body)?;
    if !expr_contains_va_arg_pack(&ret) {
        trace_inline_decision(name, "REJECT", "variadic body without va_arg_pack");
        return None;
    }
    if expr_calls_name(&ret, name) {
        trace_inline_decision(name, "REJECT", "forced variadic but direct recursion");
        return None;
    }
    let bindings: Vec<(String, Expr)> = f
        .params
        .iter()
        .zip(args.iter())
        .filter_map(|(p, a)| p.name.as_ref().map(|id| (id.name.clone(), a.clone())))
        .collect();
    if bindings.len() != f.params.len() {
        return None; // 无形名实参 (如 `void f(int, ...)`): 无法绑定
    }
    let pack_args = &args[f.params.len()..];
    trace_inline_decision(name, "INLINE", "always_inline variadic + va_arg_pack");
    Some(substitute_params(ret, &bindings, pack_args))
}

/// 取 always_inline 函数体的返回值表达式。除单 `Return` 外, 还认
/// `[If{c, then: Return(a), else: None}, Return(b)]` (≡ `return c ? a : b;`)
/// —— 变参 va_arg_pack 转发函数的典型形状 (va-arg-pack-1 的 bar)。
fn inline_body_return_expr(body: &Block) -> Option<Expr> {
    if let [BlockItem::Stmt(Stmt::Return { val: Some(ret), .. })] = body.items.as_slice() {
        return Some(ret.clone());
    }
    if let [
        BlockItem::Stmt(Stmt::If {
            cond,
            then_,
            else_: None,
            span,
        }),
        BlockItem::Stmt(Stmt::Return { val: Some(ret2), .. }),
    ] = body.items.as_slice()
    {
        let then_ret = match then_.as_ref() {
            Stmt::Return { val: Some(v), .. } => v.clone(),
            Stmt::Block { b, .. } => match b.items.as_slice() {
                [BlockItem::Stmt(Stmt::Return { val: Some(v), .. })] => v.clone(),
                _ => return None,
            },
            _ => return None,
        };
        return Some(Expr::Cond {
            cond: Box::new(cond.clone()),
            then_: Box::new(then_ret),
            else_: Box::new(ret2.clone()),
            span: span.clone(),
            ty: ret2.ty(),
        });
    }
    None
}

/// 表达式里是否出现 `__builtin_va_arg_pack ()` (递归扫描, 含 Call 实参表)。
fn expr_contains_va_arg_pack(e: &Expr) -> bool {
    match e {
        Expr::VaArgPack { .. } => true,
        Expr::Call { callee, args, .. } => {
            expr_contains_va_arg_pack(callee) || args.iter().any(expr_contains_va_arg_pack)
        },
        Expr::Binop { l, r, .. } => {
            expr_contains_va_arg_pack(l) || expr_contains_va_arg_pack(r)
        },
        Expr::Unary { e, .. } | Expr::Cast { e, .. } => expr_contains_va_arg_pack(e),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_contains_va_arg_pack(cond)
                || expr_contains_va_arg_pack(then_)
                || expr_contains_va_arg_pack(else_)
        },
        Expr::Comma { l, r, .. } => {
            expr_contains_va_arg_pack(l) || expr_contains_va_arg_pack(r)
        },
        Expr::Assign { l, r, .. } => {
            expr_contains_va_arg_pack(l) || expr_contains_va_arg_pack(r)
        },
        _ => false,
    }
}

fn inline_expr(
    e: Expr,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
) -> Expr {
    let e = match e {
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if let Some(inlined) = try_inline_call(
                    &name.name,
                    &args,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    depth,
                ) {
                    // 内联后继续递归内联替换结果中的嵌套调用
                    return inline_expr(
                        inlined,
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth + 1,
                    );
                }
            }
            Expr::Call {
                callee: Box::new(inline_expr(
                    *callee,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                )),
                args: args
                    .into_iter()
                    .map(|a| {
                        inline_expr(
                            a,
                            funcs,
                            level,
                            caller_flatten,
                            caller_is_hot,
                            fenv_access,
                            depth,
                        )
                    })
                    .collect(),
                span,
                ty,
            }
        },
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op,
            l: Box::new(inline_expr(
                *l,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            r: Box::new(inline_expr(
                *r,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            span,
            ty,
        },
        Expr::Unary { op, e, span, ty } => Expr::Unary {
            op,
            e: Box::new(inline_expr(
                *e,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            span,
            ty,
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            span,
            ty,
        } => Expr::Cond {
            cond: Box::new(inline_expr(
                *cond,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            then_: Box::new(inline_expr(
                *then_,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            else_: Box::new(inline_expr(
                *else_,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            span,
            ty,
        },
        other => other,
    };
    fold_expr(e, fenv_access)
}

fn is_side_effect_free(e: &Expr) -> bool {
    // Volatile-typed expressions always have observable side effects
    if crate::ast::type_has_volatile(&e.ty()) {
        return false;
    }
    matches!(
        e,
        Expr::Int { .. }
            | Expr::SizeofTy { .. }
            | Expr::SizeofExpr { .. }
            | Expr::AlignofTy { .. }
            | Expr::AlignofExpr { .. }
            | Expr::Offsetof { .. }
    )
}

trait StmtSpan {
    fn span(&self) -> &Span;
}

impl StmtSpan for Stmt {
    fn span(&self) -> &Span {
        match self {
            Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Do { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::Block { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Goto { span, .. }
            | Stmt::GotoIndirect { span, .. }
            | Stmt::Label { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::AsmGoto { span, .. }
            | Stmt::Invoke { span, .. }
            | Stmt::EhLandingPad { span, .. }
            | Stmt::Fallthrough { span } => span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ident, Type};
    use crate::diagnostics::Span;
    use crate::ir;

    fn int_ty() -> Type {
        Type::Int
    }

    #[test]
    fn o2_folds_constant_if() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident {
                    name: "main".into(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![BlockItem::Stmt(Stmt::If {
                        cond: Expr::Int {
                            v: 1,
                            span: Span::point("t.c", 2, 1),
                            ty: int_ty(),
                        },
                        then_: Box::new(Stmt::Return {
                            val: Some(Expr::Int {
                                v: 7,
                                span: Span::point("t.c", 3, 1),
                                ty: int_ty(),
                            }),
                            span: Span::point("t.c", 3, 1),
                        }),
                        else_: None,
                        span: Span::point("t.c", 2, 1),
                    })],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_static: false,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
                is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None,
                dtor_priority: None,
                is_retain: false,
                is_used_attr: false,
                is_flatten: false,
                error_msg: None,
                warning_msg: None,
                deprecated_msg: None,
                ifunc_resolver: None,
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
                is_naked: false,
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            assert_eq!(f.body.as_ref().unwrap().items.len(), 1);
            assert!(matches!(
                f.body.as_ref().unwrap().items[0],
                BlockItem::Stmt(Stmt::Return { .. })
            ));
        }
    }

    #[test]
    fn o2_float_fold_respects_fenv_access() {
        let float_expr = || Expr::Binop {
            op: BinOp::Add,
            l: Box::new(Expr::Float {
                c: crate::ast::FloatConst::F64(1.5),
                span: Span::point("t.c", 1, 1),
                ty: Type::Double,
            }),
            r: Box::new(Expr::Float {
                c: crate::ast::FloatConst::F64(2.25),
                span: Span::point("t.c", 1, 1),
                ty: Type::Double,
            }),
            span: Span::point("t.c", 1, 1),
            ty: Type::Double,
        };
        // 默认 (FENV_ACCESS OFF): 浮点常量运算被折叠
        let folded = fold_expr(float_expr(), false);
        assert!(
            matches!(folded, Expr::Float { c: crate::ast::FloatConst::F64(v), .. } if (v - 3.75).abs() < 1e-12),
            "float binop should fold without FENV_ACCESS, got {folded:?}"
        );
        // FENV_ACCESS ON: 保留运行时二元运算 (舍入模式可能在运行时改变)
        let kept = fold_expr(float_expr(), true);
        assert!(
            matches!(kept, Expr::Binop { .. }),
            "float binop must not fold under FENV_ACCESS, got {kept:?}"
        );
    }

    #[test]
    fn o2_preserves_goto_label_after_return() {
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident {
                    name: "f".into(),
                    span: Span::point("t.c", 1, 1),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![
                        BlockItem::Stmt(Stmt::If {
                            cond: Expr::Ident {
                                name: Ident {
                                    name: "x".into(),
                                    span: Span::point("t.c", 2, 1),
                                },
                                span: Span::point("t.c", 2, 1),
                                ty: int_ty(),
                            },
                            then_: Box::new(Stmt::Goto {
                                target: Ident {
                                    name: "found".into(),
                                    span: Span::point("t.c", 3, 1),
                                },
                                span: Span::point("t.c", 3, 1),
                            }),
                            else_: None,
                            span: Span::point("t.c", 2, 1),
                        }),
                        BlockItem::Stmt(Stmt::Return {
                            val: Some(Expr::Int {
                                v: 0,
                                span: Span::point("t.c", 4, 1),
                                ty: int_ty(),
                            }),
                            span: Span::point("t.c", 4, 1),
                        }),
                        BlockItem::Stmt(Stmt::Label {
                            name: Ident {
                                name: "found".into(),
                                span: Span::point("t.c", 5, 1),
                            },
                            stmt: Box::new(Stmt::Return {
                                val: Some(Expr::Int {
                                    v: 1,
                                    span: Span::point("t.c", 6, 1),
                                    ty: int_ty(),
                                }),
                                span: Span::point("t.c", 6, 1),
                            }),
                            span: Span::point("t.c", 5, 1),
                        }),
                    ],
                    span: Span::point("t.c", 1, 1),
                }),
                span: Span::point("t.c", 1, 1),
                is_static: false,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
                is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None,
                dtor_priority: None,
                is_retain: false,
                is_used_attr: false,
                is_flatten: false,
                error_msg: None,
                warning_msg: None,
                deprecated_msg: None,
                ifunc_resolver: None,
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
                is_naked: false,
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            assert_eq!(
                f.body.as_ref().unwrap().items.len(),
                3,
                "label after return must survive DCE"
            );
            assert!(matches!(
                f.body.as_ref().unwrap().items[2],
                BlockItem::Stmt(Stmt::Label { .. })
            ));
        }
    }

    /// G2：`dce_block` 不得把 Label 后面到本 block 末尾（或下一个顶层 Return）
    /// 之间的顶层 item 当不可达清掉。旧实现只保 Label 本身、把 `out: <body>; return s;`
    /// 里的 `return s;` 清掉 ⇒ `goto out` 从 out 直落进 late 再从函数尾 fall-off。
    ///
    /// 直接对 `dce_block` 单元测（不走完整 `optimize`），避免其他 pass 混进信号。
    /// 修复前：items.len()==5（两个顶层 Return 被清）；修复后：items.len()==7。
    #[test]
    fn g2_dce_block_retains_items_after_label() {
        let src = b"int f(int n) {\n\
                      int s = n;\n\
                      goto out;\n\
                      return 999;\n\
                    out:\n\
                      s -= 3;\n\
                      return s;\n\
                    late:\n\
                      s -= 5;\n\
                      return s;\n\
                  }\n";
        let (prog, diags) = crate::parser::parse(src, "g2-dce.c");
        assert!(
            diags
                .iter()
                .all(|d| !matches!(d.sev, crate::diagnostics::Severity::Error)),
            "parse errors: {:?}",
            diags
        );
        let f = prog
            .decls
            .iter()
            .find_map(|d| {
                if let Decl::Func(f) = d {
                    if f.name.name == "f" {
                        Some(f)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("func f");
        let mut items = f.body.as_ref().unwrap().items.clone();
        // 预处理：确认入参形状已如预期（Goto + Return + Label + Return + Label + Return）。
        let pre_returns = items
            .iter()
            .filter(|it| matches!(it, BlockItem::Stmt(Stmt::Return { .. })))
            .count();
        let pre_labels = items
            .iter()
            .filter(|it| matches!(it, BlockItem::Stmt(Stmt::Label { .. })))
            .count();
        assert_eq!(pre_labels, 2, "夹具应含 out: / late: 两个 Label");
        assert!(pre_returns >= 3, "夹具应有 ≥ 3 个顶层 Return (999/s/s)，实得 {pre_returns}");

        dce_block(&mut items);

        let post_returns = items
            .iter()
            .filter(|it| matches!(it, BlockItem::Stmt(Stmt::Return { .. })))
            .count();
        let post_labels = items
            .iter()
            .filter(|it| matches!(it, BlockItem::Stmt(Stmt::Label { .. })))
            .count();
        assert_eq!(post_labels, 2, "两个 Label 必须保留");
        assert!(
            post_returns >= 3,
            "G2: Label 后面的顶层 Return 不能当不可达清掉 — 实得 {post_returns} 个 Return, 得 ≥ 3 (999 + out后的 s + late后的 s)"
        );
    }

    /// 修 G2 后仍须确保：**没有 Label** 时，Return 后的真不可达仍被清掉（dce 不能完全不删）。
    #[test]
    fn g2_dce_block_still_prunes_pure_dead_after_return() {
        let src = b"int f(int n) {\n\
                      if (n == 0) {\n\
                          return 1;\n\
                          n = 42;\n\
                          n = 43;\n\
                      }\n\
                      return 2;\n\
                  }\n";
        let (prog, _diags) = crate::parser::parse(src, "g2-dce-still.c");
        let f = prog
            .decls
            .iter()
            .find_map(|d| {
                if let Decl::Func(f) = d {
                    if f.name.name == "f" {
                        Some(f)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("func f");
        // 直接将 内层 If-then block 拉出来 dce。
        // 入参 then_ block: [Return 1, n=42, n=43] → dce 后: [Return 1] (1 个)。
        let mut found_then_items: Option<Vec<BlockItem>> = None;
        for it in &f.body.as_ref().unwrap().items {
            if let BlockItem::Stmt(Stmt::If { then_, .. }) = it {
                if let Stmt::Block { b, .. } = &**then_ {
                    found_then_items = Some(b.items.clone());
                    break;
                }
            }
        }
        let mut then_items = found_then_items.expect("if-then block");
        assert!(
            then_items.len() >= 3,
            "夹具内部预期 [Return 1, n=42, n=43], 实得 len={}",
            then_items.len()
        );
        dce_block(&mut then_items);
        assert_eq!(
            then_items.len(),
            1,
            "G2 修后仍需将 Return 后的**非 Label** 不可达清掉 (len 应为 1)"
        );
    }

    #[test]
    fn o1_inlines_always_inline_with_params() {
        let span = Span::point("t.c", 1, 1);
        let int = int_ty();
        let x_ident = || Expr::Ident {
            name: Ident {
                name: "x".into(),
                span: span.clone(),
            },
            span: span.clone(),
            ty: int.clone(),
        };
        let sq = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident {
                name: "sq".into(),
                span: span.clone(),
            },
            params: vec![crate::ast::Param {
                quals: crate::ast::QualifierFlags::NONE,
                ty: int.clone(),
                name: Some(Ident {
                    name: "x".into(),
                    span: span.clone(),
                }),
                span: span.clone(),
                vla_dim: None,
            }],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Binop {
                        op: BinOp::Mul,
                        l: Box::new(x_ident()),
                        r: Box::new(x_ident()),
                        span: span.clone(),
                        ty: int.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: true,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: true,
            is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        });
        let main_fn = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident {
                name: "main".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: Ident {
                                name: "sq".into(),
                                span: span.clone(),
                            },
                            span: span.clone(),
                            ty: int.clone(),
                        }),
                        args: vec![Expr::Int {
                            v: 5,
                            span: span.clone(),
                            ty: int.clone(),
                        }],
                        span: span.clone(),
                        ty: int.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        });
        let prog = Program {
            decls: vec![sq, main_fn],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 1);
        let main_body = match &module.source().decls[1] {
            Decl::Func(f) => f.body.as_ref().unwrap(),
            _ => panic!("expected main"),
        };
        match &main_body.items[0] {
            BlockItem::Stmt(Stmt::Return {
                val: Some(Expr::Int { v: 25, .. }),
                ..
            }) => {},
            other => panic!("expected inlined constant 25, got {other:?}"),
        }
    }

    #[test]
    fn o2_preserves_label_in_dead_if_branch() {
        // `if (0) { lbl: ... }` 中的标签可被 goto 跳入,
        // 常量分支折叠不能丢弃它 (否则 codegen unresolved label 崩溃)。
        let span = Span::point("t.c", 1, 1);
        let int = int_ty();
        let func = Decl::Func(FuncDecl {
            ret_ty: int.clone(),
            name: Ident {
                name: "f".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::If {
                    cond: Expr::Int {
                        v: 0,
                        span: span.clone(),
                        ty: int.clone(),
                    },
                    then_: Box::new(Stmt::Block {
                        b: Block {
                            items: vec![BlockItem::Stmt(Stmt::Label {
                                name: Ident {
                                    name: "undo".into(),
                                    span: span.clone(),
                                },
                                stmt: Box::new(Stmt::Expr {
                                    e: Expr::Int {
                                        v: 1,
                                        span: span.clone(),
                                        ty: int.clone(),
                                    },
                                    span: span.clone(),
                                }),
                                span: span.clone(),
                            })],
                            span: span.clone(),
                        },
                        span: span.clone(),
                    }),
                    else_: None,
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        });
        let prog = Program {
            decls: vec![func],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, 2);
        let body = &module.source().decls[0];
        if let Decl::Func(f) = body {
            let items = f.body.as_ref().unwrap().items.clone();
            let has_label = items.iter().any(|item| match item {
                BlockItem::Stmt(Stmt::If { then_, .. }) => stmt_has_label(then_),
                BlockItem::Stmt(s) => stmt_has_label(s),
                _ => false,
            });
            assert!(has_label, "label in dead if branch must survive -O2 fold");
        }
    }

    // -------------------------------------------------------------------
    // Inline policy tests (parser-driven for conciseness)
    // -------------------------------------------------------------------

    /// Helper: parse source, lower, optimize at given level, return Program.
    fn optimize_src(src: &[u8], level: u8) -> Program {
        let (prog, diags) = crate::parser::parse(src, "inline-test.c");
        assert!(
            diags
                .iter()
                .all(|d| !matches!(d.sev, crate::diagnostics::Severity::Error)),
            "parse errors: {:?}",
            diags
        );
        let (mut module, _) = ir::lower(prog).expect("lower");
        optimize(&mut module, level);
        module.source().clone()
    }

    /// Check if main() body contains a Call expression (i.e. not inlined).
    fn main_has_call(prog: &Program) -> bool {
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.name.name == "main" {
                    if let Some(b) = &f.body {
                        return block_has_call(b);
                    }
                }
            }
        }
        false
    }

    fn block_has_call(b: &Block) -> bool {
        b.items.iter().any(|it| match it {
            BlockItem::Stmt(s) => stmt_has_call(s),
            _ => false,
        })
    }

    fn stmt_has_call(s: &Stmt) -> bool {
        match s {
            Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => expr_has_call(e),
            Stmt::Block { b, .. } => block_has_call(b),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                expr_has_call(cond)
                    || stmt_has_call(then_)
                    || else_.as_ref().is_some_and(|e| stmt_has_call(e))
            },
            _ => false,
        }
    }

    fn expr_has_call(e: &Expr) -> bool {
        match e {
            Expr::Call { .. } => true,
            Expr::Binop { l, r, .. } => expr_has_call(l) || expr_has_call(r),
            Expr::Unary { e, .. } | Expr::Cast { e, .. } => expr_has_call(e),
            Expr::Cond {
                cond, then_, else_, ..
            } => expr_has_call(cond) || expr_has_call(then_) || expr_has_call(else_),
            _ => false,
        }
    }

    #[test]
    fn noinline_is_respected_at_o1() {
        // noinline function must NOT be inlined even if small
        let prog = optimize_src(
            b"__attribute__((noinline)) static int f(int x) { return x + 1; }\n\
              int main(void) { return f(5); }\n",
            1,
        );
        assert!(main_has_call(&prog), "noinline function should not be inlined");
    }

    #[test]
    fn always_inline_large_body_inlined_at_o0() {
        // always_inline with body > 8 items must still be inlined at O0
        let prog = optimize_src(
            b"__attribute__((always_inline)) static inline int big(int x) {\n\
              int a = x+1; int b = a+2; int c = b+3; int d = c+4;\n\
              int e = d+5; int f = e+6; int g = f+7; int h = g+8;\n\
              int i = h+9; return i;\n\
              }\n\
              int main(void) { return big(0); }\n",
            0,
        );
        assert!(
            !main_has_call(&prog),
            "always_inline must be inlined at O0 regardless of body size"
        );
    }

    #[test]
    fn always_inline_plus_noinline_always_inline_wins() {
        // When both always_inline and noinline are set, always_inline wins (GCC)
        let prog = optimize_src(
            b"__attribute__((always_inline, noinline)) static inline int f(int x) {\n\
              return x * 2;\n\
              }\n\
              int main(void) { return f(3); }\n",
            1,
        );
        assert!(!main_has_call(&prog), "always_inline should win over noinline");
    }

    #[test]
    fn o0_does_not_inline_regular_static_inline() {
        // Regular static inline must NOT be inlined at O0
        let prog = optimize_src(
            b"static inline int f(int x) { return x + 1; }\n\
              int main(void) { return f(5); }\n",
            0,
        );
        assert!(main_has_call(&prog), "regular static inline should not be inlined at O0");
    }

    #[test]
    fn expr_node_count_simple() {
        // 5 + 3 => Binop(5, 3) = 3 nodes
        let e = Expr::Binop {
            op: BinOp::Add,
            l: Box::new(Expr::Int {
                v: 5,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: 3,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }),
            span: Span::point("t", 1, 1),
            ty: int_ty(),
        };
        assert_eq!(expr_node_count(&e), 3);
    }

    #[test]
    fn expr_node_count_ident() {
        let e = Expr::Ident {
            name: Ident {
                name: "x".into(),
                span: Span::point("t", 1, 1),
            },
            span: Span::point("t", 1, 1),
            ty: int_ty(),
        };
        assert_eq!(expr_node_count(&e), 1);
    }

    #[test]
    fn estimate_inline_cost_small_func() {
        // A simple return x+1 function should have low cost
        let (prog, _) =
            crate::parser::parse(b"static int f(int x) { return x + 1; }\n", "cost-test.c");
        if let Decl::Func(f) = &prog.decls[0] {
            let body = f.body.as_ref().unwrap();
            let args = vec![Expr::Int {
                v: 5,
                span: Span::point("t", 1, 1),
                ty: int_ty(),
            }];
            let cost = estimate_inline_cost(f, body, &args);
            assert!(
                cost <= INLINE_COST_THRESHOLD,
                "small function cost {} should be <= {}",
                cost,
                INLINE_COST_THRESHOLD
            );
        } else {
            panic!("expected FuncDecl");
        }
    }

    /// Profile-guided inline: hot caller at O2 should relax the
    /// heuristic (allow non-static, higher cost threshold).
    #[test]
    fn profile_guided_inline_hot_caller_relaxed_threshold() {
        let sp = Span::point("t.c", 1, 1);
        // callee: non-static, 2 params, body cost = 3 (return x+y)
        let callee = FuncDecl {
            ret_ty: int_ty(),
            name: Ident {
                name: "add".into(),
                span: sp.clone(),
            },
            params: vec![
                crate::ast::Param {
                    quals: crate::ast::QualifierFlags::NONE,
                    name: Some(Ident {
                        name: "x".into(),
                        span: sp.clone(),
                    }),
                    ty: int_ty(),
                    span: sp.clone(),
                    vla_dim: None,
                },
                crate::ast::Param {
                    quals: crate::ast::QualifierFlags::NONE,
                    name: Some(Ident {
                        name: "y".into(),
                        span: sp.clone(),
                    }),
                    ty: int_ty(),
                    span: sp.clone(),
                    vla_dim: None,
                },
            ],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Binop {
                        op: BinOp::Add,
                        l: Box::new(Expr::Ident {
                            name: Ident {
                                name: "x".into(),
                                span: sp.clone(),
                            },
                            span: sp.clone(),
                            ty: int_ty(),
                        }),
                        r: Box::new(Expr::Ident {
                            name: Ident {
                                name: "y".into(),
                                span: sp.clone(),
                            },
                            span: sp.clone(),
                            ty: int_ty(),
                        }),
                        span: sp.clone(),
                        ty: int_ty(),
                    }),
                    span: sp.clone(),
                })],
                span: sp.clone(),
            }),
            span: sp.clone(),
            is_static: false,
            is_extern: false,
            is_variadic: false,
            is_weak: false,
            is_constructor: false,
            is_destructor: false,
            is_nested: false,
            is_cold: false,
            is_hot: false,
            section: None,
            alignment: None,
            visibility: None,
            alias_name: None,
            upvars: vec![],
            is_unused: false,
            is_noinline: false,
            is_always_inline: false,
            is_inline: false,
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        };

        let mut funcs = HashMap::new();
        funcs.insert("add".into(), callee.clone());
        let args = vec![
            Expr::Int {
                v: 1,
                span: sp.clone(),
                ty: int_ty(),
            },
            Expr::Int {
                v: 2,
                span: sp.clone(),
                ty: int_ty(),
            },
        ];

        // 内联决策读的是 `InlineTable`（函数表 + 调用点数 + asm 引用名），测试侧同样
        // 打包一份，不让测试走另一条「只有函数表」的路径；本程序没有内联汇编，
        // 所以 asm 名单为空。
        let table = InlineTable {
            sites: count_direct_call_sites(&funcs),
            asm_names: std::collections::HashSet::new(),
            funcs,
        };

        // O2 + caller_is_hot=false: non-static with params should REJECT
        let result_cold = try_inline_call("add", &args, &table, 2, false, false, 0);
        assert!(
            result_cold.is_none(),
            "O2 cold caller should reject non-static callee with params"
        );

        // O2 + caller_is_hot=true: profile-relaxed should ACCEPT
        let result_hot = try_inline_call("add", &args, &table, 2, false, true, 0);
        assert!(
            result_hot.is_some(),
            "O2 hot caller should inline non-static callee (profile-relaxed)"
        );
    }
}
