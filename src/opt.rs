// src/opt.rs - typed IR 优化 passes
// 第一性原理: 优化在 IR 层做数据流/常量传播，后端只负责指令选择。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::ast::{
    AddrSpace, ArraySize, BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, InitItem, Program,
    QualifierFlags, Stmt, Type, VarDecl,
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

/// 「强制内联」（`always_inline` / `flatten`）的深度上限，与投机的 8 层分开。
///
/// 为什么必须分开：`__always_inline` 是**语义**要求（GCC 展开不了就直接报错），
/// 而内核 `mm/memory.c` 那条链自己就有 7 层：
/// `copy_p4d_range → copy_pud_range → copy_pmd_range → copy_pte_range →
///  folio_dup_file_rmap_* → __folio_dup_file_rmap → __folio_rmap_sanity_checks`，
/// 中间任何一层再叠一个局部助手就撞 8 层线。撞线的后果不是「少一个优化」而是**多留
/// 一份外带副本**：副本里 `switch (level) { … default: BUILD_BUG(); }` 的选择子是
/// **运行时形参**，`default` 永远可达 ⇒ 产物里留下 `__compiletime_assert_NNN` 的
/// 调用 ⇒ 链接期未定义引用（实测 `mm/memory.o` 3 条；gcc 侧这两个名字连定义都不存在）。
///
/// 仍不是无限（24）：真正的无限展开只可能来自 `always_inline` 互递归，而那在 GCC 里
/// 本来就是编译错误；内核实测最深一条 ≤ 10 层，撞到 24 说明有环，应当拒绝。
const INLINE_MAX_DEPTH_FORCED: u32 = 24;

// ---------------------------------------------------------------------------
// Inline decision tracing
//
// Set DCC_INLINE_TRACE=1 in the environment to emit one line per inline
// decision to stderr.  Useful for debugging kernel builds where
// __always_inline / noinline correctness is critical.
// ---------------------------------------------------------------------------

static INLINE_TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

/// `DCC_REBUILD_DUMP=1` 诊断窗口：打印 `rebuild_block` 产出的函数体 AST；
/// `DCC_REBUILD_DUMP_FN=<名>` 只打印指定函数。与 `DCC_DUPARG_DUMP`/
/// `DCC_MEMDBG` 同风格。重建后的 AST 是「IR 已走完、汇编已太远」的盲区：
/// torture pr104814 的「`if(c) break else break` 被发在内层循环」就是靠它看到的。
/// 用 `OnceLock` 缓存：本判据在**每个函数**的重建点上求值，`env::var` 每次都要拿锁。
static REBUILD_DUMP: OnceLock<(bool, Option<String>)> = OnceLock::new();

fn rebuild_dump_cfg() -> &'static (bool, Option<String>) {
    REBUILD_DUMP.get_or_init(|| {
        (
            std::env::var("DCC_REBUILD_DUMP").is_ok(),
            std::env::var("DCC_REBUILD_DUMP_FN").ok(),
        )
    })
}

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
/// - speculative 内联深度上限 `INLINE_MAX_DEPTH` (8)，强制内联另有
///   `INLINE_MAX_DEPTH_FORCED` (24) —— 两者不能共用一条线，理由见后者的注释。
///   递归函数通过 callee-name 检测阻止直接递归内联。
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
    optimize_with_full_target(module, level, has_vnni, vec_bits, true, false, true, false);
}

/// 带完整目标信息的优化入口。
///
/// `strict_aliasing`: -fno-strict-aliasing 时为 false, 别名分析合并所有指针
/// `strict_overflow`: -fno-strict-overflow 时为 false, 禁止依赖有符号不溢出的推理
pub fn optimize_with_full_target(
    module: &mut Module,
    level: u8,
    has_vnni: bool,
    vec_bits: u32,
    strict_aliasing: bool,
    debug_info: bool,
    strict_overflow: bool,
    pic: bool,
) {
    // 比较恒等式折叠: `(a < b) != (b > a)` 这类关系恒等式在**任何** -O 档都要折掉,
    // 与 GCC 前端 fold-const 一致; 否则 `link_error()` 之类的死调用会留在产物里
    // (gcc.dg pr27302 / pr27302-2 在默认档位链接失败)。必须排在 level==0 提前返回之前。
    // 体检（`DCC_PASS_STATS=1`，见 `pass_probe` 模块）：记下 -O 档并清空上一模块的累计。
    crate::pass_probe::begin(level);
    if !opt_skipped("fold_compare_identities") {
        crate::pass_probe::probe("fold_compare_identities", module, |m| {
            crate::ast_cmp_fold::run(m, strict_overflow)
        });
    }
    // 「体只有 `return <编译期常量>;` 的 static 桩」的调用直接换成那个常量。
    //
    // **必须排在 `function_inline` 之前**（以前放在它之后，是错的，实测推翻）：
    // `static inline unsigned long stub(void) { return 0UL; }` + `if (stub()) trap();`
    // 交给内联先跑，调用点就不再有 `Call` 节点了 —— 内联展开成一个语句块，里面
    // `VarDecl __inl0$rv`（无初值）+ `__inl0$rv += 0;`，条件位置读的是这个**嵌块内的
    // 临时量**（`/tmp/kwred/w.c` + `DCC_DUMP_PASS=fold_const_return_calls` 实测），
    // 于是条件位置的常量桩引用一条都消不掉。内核里配置关掉的桩绝大多数是
    // `static inline` 一行 `return 0;/return NULL;`（`sme_get_me_mask()`、`bio_integrity()`、
    // `__blk_crypto_*` 守的那批），全部落在这条路上 ⇒ 换成先代回常量、再内联。
    // `-O0` 不做（保持「调试时看得见控制流」的原契约，与下面 `level == 0` 提前返回一致）。
    if level > 0 && !opt_skipped("fold_const_return_calls") {
        crate::pass_probe::probe("fold_const_return_calls", module, fold_const_return_calls);
        dump_after_pass("fold_const_return_calls", module);
    }
    // 内联: O0-O2 仅 always_inline/flatten; O3 额外启发式
    if !opt_skipped("function_inline") {
        crate::pass_probe::probe("function_inline", module, |m| function_inline(m, level));
        dump_after_pass("function_inline", module);
    }
    // asm 只立即数操作数的常量代回：必须紧跟内联（内联产生的形参临时量就是它的主客户），
    // 而且必须在 `level == 0` 提前返回**之前** —— `-O0` 下 `always_inline` 照样会被展开，
    // 不代回就只能拿到寄存器（产物里的 `.altinstructions`/`__bug_table` 条目会坏掉，
    // 而编译本身不报错）。
    if !opt_skipped("fold_asm_immediate_consts") {
        crate::pass_probe::probe("fold_asm_immediate_consts", module, fold_asm_immediate_consts);
    }
    // `static inline` 的定义就是内联体：调用点全展开后没人引用就不发副本。
    // 必须排在内联之后（判据看的是展开后还剩不剩引用），且必须在 `level == 0`
    // 提前返回**之前** —— 内核 `arch_static_branch` 这类 `"i"(形参)` 的
    // `__always_inline` 助手在 -O0/-O1 没有跨函数 DCE，副本会直接编不出来。
    if !opt_skipped("drop_unused_static_inline_defs") {
        crate::pass_probe::probe(
            "drop_unused_static_inline_defs",
            module,
            drop_unused_static_inline_defs,
        );
        dump_after_pass("drop_unused_static_inline_defs", module);
    }
    if level == 0 {
        crate::pass_probe::flush("");
        return;
    }
    // 常量桩的代回在内联**之前**已经跑过一次（见上面的理由：晚于内联就只能看见
    // `__inl0$rv` 这种嵌块临时量，看不见桩调用）。这里再跑一次是便宜的：内联可能把
    // 新的桩调用带进原本不可达的位置，后面的 `while (0)` / `if (0)` 判据需要它们。
    if !opt_skipped("fold_const_return_calls") {
        crate::pass_probe::probe("fold_const_return_calls", module, fold_const_return_calls);
        dump_after_pass("fold_const_return_calls", module);
    }
    // 同 TU 内「空体 void 函数」的调用就地吸收（上一行只管 `static` 常量桩，这一行管
    // 非 static、无返回值的空壳；两者合起来才能把内核那一族条件编译的空实现从产物里清掉）。
    if !opt_skipped("fold_empty_void_calls") {
        crate::pass_probe::probe("fold_empty_void_calls", module, fold_empty_void_calls);
        dump_after_pass("fold_empty_void_calls", module);
    }
    // 常量桩折完后的第二跳：常量现在坐在局部里，循环条件读的是变量，
    // 不把它代回条件/选择子位置，`while (0)` / `if (0)` 删不掉（§3.41：这是
    // 全树剩下 253 行未定义引用的主机制）。必须排在死分支折叠之前。
    if !opt_skipped("propagate_const_locals_in_conditions") {
        crate::pass_probe::probe(
            "propagate_const_locals_in_conditions",
            module,
            propagate_const_locals_in_conditions,
        );
        dump_after_pass("propagate_const_locals_in_conditions", module);
    }
    // IPA modref: 跨过程 "哪些指针形参被写穿" 摘要 + 只对「地址逃逸点全部是已证明
    // 不写的调用实参」的局部折叠 __builtin_constant_p。必须排在 fold_constants 之前:
    // 折出的 Int(1) 之后的 fold_expr / SCCP 才能把 `!1` 折成 0、删掉 `if(!1) abort();`。
    if level >= 2 && !opt_skipped("ipa_modref") {
        crate::pass_probe::probe("ipa_modref", module, |m| crate::ipa_modref::run(m, level));
        dump_after_pass("ipa_modref", module);
    }
    if !opt_skipped("fold_constants") {
        crate::pass_probe::probe("fold_constants", module, fold_constants);
        dump_after_pass("fold_constants", module);
    }
    // 死分支折叠与不可达截断从 **-O1** 起就要跑（以前放在 `level >= 2` 里）。
    // 为什么不能等 -O2：内核那一族「编译期尺寸检查」宏（`__cmpxchg_wrong_size` /
    // `__xadd_wrong_size` / `__bad_size_call_parameter` / `__get_user_bad` …）是**故意不定义**
    // 的函数，完全指望死代码消除把引用弄消失；gcc 连 -O0 都会把常量 switch 折掉，
    // 而 dcc 在 -O1 不折就是链接期 `undefined reference to 'trap_d'`（集成用例
    // `const_switch_fold_handles_braced_arm_and_fallthrough_chain` 跑 -O1 时实测到）。
    // -O0 仍在上面提前 return，保留「调试时看得见控制流」的原契约。
    // `fold_constant_branches`/`eliminate_unreachable` 里「恒假分支整个抽掉」顺带完成了
    // dead asm elimination：纯 asm（非 volatile、无输出、无 memory clobber）在结果没人用
    // 时随臂一起消失，与 gcc 同；volatile 与有副作用的 asm 永远保留。
    if !opt_skipped("fold_constant_branches") {
        crate::pass_probe::probe("fold_constant_branches", module, fold_constant_branches);
        dump_after_pass("fold_constant_branches", module);
    }
    if !opt_skipped("eliminate_unreachable") {
        crate::pass_probe::probe("eliminate_unreachable", module, eliminate_unreachable);
        dump_after_pass("eliminate_unreachable", module);
    }
    // 第二跳：上一轮死分支/不可达截除之后，**剩下的写入才可能只剩一个**。
    // 实测形状（`mm/shmem.c` 的错误路径）：
    //     pages = 1;                             /* else 分支 */
    //     while (suitable_orders) { pages = 1UL << order; … }   /* 死循环，本轮刚被删 */
    //     … else if (pages > 1) { if (pages == HPAGE_PMD_NR) 陷阱 }
    // 死循环删前，`pages` 有两个不同的写入值（1 与 `1<<order`）→ 第一跳不候选；
    // 删后就只剩 1 了 → 必须再跑一次才能把 `pages > 1` 判死（实测 `mm/shmem.o`
    // 的 530/531/532 三条就卡在这里）。两跳不引入新规则，只是把同一个判据
    // 在新事实上看一遍。
    if !opt_skipped("propagate_const_locals_in_conditions") {
        crate::pass_probe::probe(
            "propagate_const_locals_in_conditions",
            module,
            propagate_const_locals_in_conditions,
        );
    }
    if !opt_skipped("fold_constant_branches") {
        crate::pass_probe::probe("fold_constant_branches", module, fold_constant_branches);
        dump_after_pass("fold_constant_branches#2", module);
    }
    // 第三跳（§3.51）：**块内向前**的常量代回。上面两个 pass 都靠「声明初值」或 SSA 层的
    // 常量传播，而含 `goto` 的函数在 SSA 层只走 analysis-only（不重建、不跑 SCCP）——
    // 内核的错误路径全是 `goto out;`，于是「常量存进局部→后拿它当条件读」整族一条都折不掉。
    // 这里按位置补一次：`x = C` 之后的读点代回，直到 x 被写或遇到有 goto 指向的标号。
    //
    // 两跳之间会互相喂料，所以做成不动点（实测 `mm/shmem.c` 两轮收敛）：
    // 第一轮将死块删掉后，`else { pages = 1; }` 被拍平进函数体顶层 ⇒ `pages` 这才有一条常量
    // 可代 ⇒ 第二轮才能将 `pages > 1` 判死（剩下那两条陷阱）。
    for _ in 0..3 {
        if opt_skipped("propagate_const_stores_forward") {
            break;
        }
        let again = crate::pass_probe::probe_fn("propagate_const_stores_forward", || {
            propagate_const_stores_forward(module)
        });
        // 守卫排除值：`for (…; E != K; step)` 的 step 里 `case K` 不可达。与上面同一轮
        // 不动点里做：它丢掉的臂可能让别的写入只剩一个值，反过来又喂给下一轮。
        let narrowed = crate::pass_probe::probe_fn("narrow_switch_by_excluded_guard", || {
            narrow_switch_by_excluded_guard(module)
        });
        if !opt_skipped("fold_constant_branches") {
            crate::pass_probe::probe("fold_constant_branches", module, fold_constant_branches);
            // 本轮的折法（常量代回后的 `if (1) return …;`、空 switch 臂）**留下的常常是
            // 一条 `return`/`goto`**，它后面的语句本轮就该截掉。上面的
            // `eliminate_unreachable` 只跑过一次，位置在 cprop 之前，越不过这一批：
            // 现场是 `arch/x86/kernel/fpu/signal.c` 的 `convert_to_fxsr`（`if (likely(!ia32_fxstate))`
            // 代回后成为无条件 return，后面的 `convert_to_fxsr(&xstate, &env)` 还在发引用）
            // 与 `lib/vsprintf.c` 的 `rust_fmt_argument`（同一个形状）。
            if !opt_skipped("eliminate_unreachable") {
                crate::pass_probe::probe("eliminate_unreachable", module, eliminate_unreachable);
            }
        } else {
            break;
        }
        if !again && !narrowed {
            break;
        }
    }
    dump_after_pass("propagate_const_stores_forward", module);
    // 【第二轮强制内联】`ilog2()` 那类宏要等 `fold_constants`/`fold_constant_branches`
    // 选完边才露出真正的被调者（`include/linux/log2.h:156` 的
    // `… ? __ilog2_u32(n) : __ilog2_u64(n)`），而第一轮 `function_inline` 跑在它们之前
    // ⇒ 选出来的那个 `__always_inline` 调用再没人展开。`__always_inline` 是**硬契约**
    // （gcc 在这种情况直接报错），所以这里补一轮：传 level 0 ⇒ 该轮只做
    // always_inline/flatten，不带启发式，不会把代码尺寸再吹一遍（实测
    // `drivers/gpu/drm/i915/gt/uc/intel_guc_ads.o` 的 `__ilog2_u32` 调用）。
    // 补完再跑一次分支折叠与不可达截除：内联出来的体里同样可能有常量条件。
    if !opt_skipped("function_inline") {
        crate::pass_probe::probe("function_inline#2", module, |m| function_inline(m, 0));
        dump_after_pass("function_inline#2", module);
    }
    // 第二轮内联（或它之前的代回）新造的常量形状**没有任何表达式折法看过**：
    //   * `udelay(10)` 的形参经 `__inlN$_usec` 实体化，于是
    //     `if (__inlN$_usec >= 20000)` 只能在 `propagate_const_locals_in_conditions`
    //     之后才能折成 `if (0)`；
    //   * `X && 常量桩(0)` 要在 `fold_constants` 的 `fold_expr` 里才能吸成
    //     `(X, 0)`（内核 `static_branch_unlikely(&zoned_enabled) && bdev_is_zoned(...)`，
    //     `CONFIG_BLK_DEV_ZONED=n` 时右操作数是 0）。
    // 而这两个 pass 都只跑在第二轮内联**之前** ⇒ 这里按同一顺序补一轮，否则死臂里的
    // `__bad_udelay()` / `dm_zone_endio()` 全留在产物里（前者是链接期未定义引用，
    // 后者把整条 `dm_dax_ops` 链的同类问题遮在后面）。
    if !opt_skipped("fold_constants") {
        crate::pass_probe::probe("fold_constants#3", module, fold_constants);
    }
    if !opt_skipped("propagate_const_locals_in_conditions") {
        crate::pass_probe::probe(
            "propagate_const_locals_in_conditions#3",
            module,
            propagate_const_locals_in_conditions,
        );
    }
    if !opt_skipped("fold_constant_branches") {
        crate::pass_probe::probe("fold_constant_branches#3", module, fold_constant_branches);
        if !opt_skipped("eliminate_unreachable") {
            crate::pass_probe::probe("eliminate_unreachable#3", module, eliminate_unreachable);
        }
    }
    if level >= 2 {
        if !opt_skipped("prune_dead_stores") {
            crate::pass_probe::probe("prune_dead_stores", module, prune_dead_stores);
        }
        // 紧跟 `prune_dead_stores`：它削掉死写之后，被削空的声明才能被认出来。
        // 顺序不能反（反了就是「写还在、声明没了」 ⇒ 未定义名字）。
        if !opt_skipped("prune_dead_locals") {
            crate::pass_probe::probe("prune_dead_locals", module, prune_dead_locals);
        }
        if !opt_skipped("ast_gvn") {
            crate::pass_probe::probe("ast_gvn", module, |m| crate::ast_gvn::run(m, level));
            dump_after_pass("ast_gvn", module);
        }
        if !opt_skipped("loop_opt") {
            crate::pass_probe::probe("loop_opt", module, |m| {
                crate::loop_opt::run_with_target(m, level, has_vnni, vec_bits)
            });
        }
        // M16: 跨函数 DCE (call graph reachability)
        if !opt_skipped("cross_variable_dce") {
            crate::pass_probe::probe("cross_variable_dce", module, cross_variable_dce);
            dump_after_pass("cross_variable_dce", module);
        }
        if !opt_skipped("cross_function_dce") {
            crate::pass_probe::probe("cross_function_dce", module, cross_function_dce);
        }
        // Qualifier-aware optimization: const/volatile/restrict
        if !opt_skipped("qualifier_aware") {
            crate::pass_probe::probe("qualifier_aware", module, qualifier_aware_optimization);
        }
    }
    if level >= 2 {
        // M16: SSA pass pipeline (strength reduction, IVOP, TCO, etc.)
        if !opt_skipped("run_ssa_passes") {
            crate::pass_probe::probe("run_ssa_passes", module, |m| {
                run_ssa_passes(m, level, strict_aliasing, debug_info, pic)
            });
        }
    }
    crate::pass_probe::flush("");
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

/// 报障用：在**指定 pass 之后**把点名的函数 AST 打到 stderr。
/// `DCC_DUMP_PASS=function_inline,ast_gvn` 配 `DCC_DUMP_FN=sqlite3VdbeDelete`
/// （后者留空 = 一切函数）。只读不改：不参与任何判据、不进产物字节。
///
/// 为什么需要它：`-O2` 的「static body inline」展开后，某个空指针卫兵在产物里
/// 成了常量真（实测 `mov $1,%eax; test %eax,%eax`，于是 `aOp==0` 时照算
/// `&aOp[nOp-1]`，读野地址）。要定「是展开自己错还是后面某个 pass 折错」，
/// 靠逐 pass 关掉重编要十几轮（每轮一次全量重编）；直接看每个 pass 之后的树，
/// 一次编译就够。
pub(crate) fn dump_after_pass(name: &str, module: &mut Module) {
    // 与其余 `DCC_INLINE_*` 闸门同一纪律：`OnceLock` 只读一次 env。本函数在**每个函数×
    // 每个 pass** 上都会被叫一次，直接 `env::var` 就是上万次 getenv，会把配对计时的
    // 现场搅乱（而对计时无关的开关来说，那是白白改变待测量）。
    static SPEC: std::sync::OnceLock<(Vec<String>, String)> = std::sync::OnceLock::new();
    let (passes, want_fn) = SPEC.get_or_init(|| {
        let p = std::env::var("DCC_DUMP_PASS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        (p, std::env::var("DCC_DUMP_FN").unwrap_or_default())
    });
    if passes.is_empty() || !passes.iter().any(|s| s == name) {
        return;
    }
    let prog = module.source_mut();
    for decl in prog.decls.iter() {
        if let Decl::Func(f) = decl {
            if !want_fn.is_empty() && f.name.name != *want_fn {
                continue;
            }
            if f.body.is_some() {
                eprintln!("DUMP| pass={name} fn={}", f.name.name);
                eprintln!("{:#?}", f.body);
            }
        }
    }
}

/// `DCC_HOME_PARAMS=1` 的判据：哪些形参可以当家。
///
/// 为什么需要形参当家：解释器类函数（`luaV_execute(lua_State *L, CallInfo *ci)`）每步要读
/// 的热值就是那两个形参。实测本函数帧存 10.4/载 21.3 次每步（gcc 各 2.2），其中
/// `L`/`ci` 的槽就占 6.8 次载入/步；而候选真源 `original_locals` 不含形参，它们连候选
/// 都不是。按本机标定（一次载入/步 ≈ 5 ms），这项值约 34 ms。
///
/// 排除项（都是“拿了就错值”，不是保守好看）：
///  * 变参函数：`va_start` 要把入站寄存器区存下来，家在号上时这套机制还没跟它对调；
///  * volatile 限定：读写次数本身就是语义；
///  * VLA 形参与不可绑定的类型（聚合/宽值）：它们本来就走内存/成对通道；
///  * 重名或被取址：`escaped` 那一侧的判据在分配器里已经逐名跑过，这里不抢它的活。
fn home_param_names(f: &crate::ast::FuncDecl, enabled: bool) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if !enabled || f.is_variadic {
        return out;
    }
    for p in f.params.iter() {
        let Some(nm) = p.name.as_ref() else { continue };
        if p.quals.is_volatile() || p.vla_dim.is_some() {
            continue;
        }
        if !crate::ssa::regalloc::bindable_ty(&p.ty) {
            continue;
        }
        out.insert(nm.name.clone());
    }
    out
}

/// M16: 把 AST -> SSA, 跑 ssa::passes::run, 再 SSA -> AST 重建。
/// 含 goto / label / switch 的函数自动跳过 (走 is_structured 检测)。
/// SSA builder 通过 insert_loop_phis 为循环回边插入 phi 节点,
/// rebuild_cycle 将循环 CFG 重建为 while 循环 AST。
/// 注意: rebuild_cycle 对含循环函数的重建尚不完全正确,
/// 因此含循环的函数仍走 AST pass 路径。
fn run_ssa_passes(
    module: &mut Module,
    level: u8,
    strict_aliasing: bool,
    debug_info: bool,
    pic: bool,
) {
    // `DCC_HOME_PARAMS`：形参能不能进家的候选表（默认关 ⇒ 空集 ⇒ 今天的行为）。
    // 进程内只读一次：同一个 TU 里两个函数按不同规则发号，产物就没人能复现。
    static HP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let home_params_on = *HP.get_or_init(|| {
        std::env::var("DCC_HOME_PARAMS")
            .map(|s| s != "0")
            .unwrap_or(false)
    });
    let prog = module.source_mut();
    // Collect volatile global variable names - SSA passes must not
    // propagate constants through volatile accesses.
    //
    // 表本身挪到 `frame_guard::volatile_global_names`：发射侧（`compile_func` 里算的
    // `FrameGuard`）与这里必须是同一个答案，否则同一个形状在 -O0 与 -O2 下会得到
    // 不同的保守决定，而「volatile 每次访问都是可观察事件」这条语义就不成立了。
    let volatile_globals: std::collections::HashSet<String> =
        crate::frame_guard::volatile_global_names(&*prog);
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
        let mut s: std::collections::HashSet<String> = KNOWN_RETURNS_TWICE
            .iter()
            .map(|n| (*n).to_string())
            .collect();
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
            // P13 阶段6 批1：稠密 switch 换跳表分派，**只对非结构化函数做**。
            // 理由在 `ssa::structure` 本批的头注释里：结构化函数的 switch 要走
            // `lower_switches_in_func` 降级成 if 链交给 SSA 重建，那个通道重建不了
            // 合成的 label/goto/静态局部表；非结构化函数不重建，AST 改成什么样就
            // 发什么样，用的就是 Lua 解释器每天在走的 `&&label` + `goto *` 通路。
            // 开关 `DCC_SWITCHTABLE` **默认开**（与 `ssa::structure::jumptable_enabled()` 同一
            // 事实，以前这里写着「默认关」是过时的：默认值已由 15 轮同轮配对实测定为开，
            // median(r)=0.7245、95%CI [0.713,0.755]、15/15 轮同向）；`DCC_SWITCHTABLE=0` 仍是
            // 那条归因用的反向闸：批0c 实测单是布局就能让 SQLite 动 10%，链/表两臂必须
            // 能在同一二进制里交替配对。
            // PIC 下不能上表：表项是 `.quad 标号` 的**绝对** 64 位地址（
            // `R_X86_64_64`）。EFI stub 用 `-fPIC` 编，它禁止任何绝对重定位
            // （`libstub/Makefile` 的 STUBCPY 发现 vsprintf.o 的 `.data` 表就
            // 直接报 “absolute symbol references not allowed in the EFI stub”）；
            // PIC 的语义也不允许把地址固定在表里。退回比较链分派。
            // 非 PIC（内核主体 `-fno-PIE`）不变，跳表照旧。
            if unstructured && !pic && crate::ssa::lower_switches_to_jumptable_in_func(f) {
                if std::env::var("DCC_SWITCHTABLE_DUMP").is_ok() {
                    eprintln!("dcc: switch 跳表已降级：{}", f.name.name);
                }
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
                            home_params: home_param_names(f, home_params_on),
                        };
                        let plan = f
                            .body
                            .as_ref()
                            .map(|orig| crate::ssa::regalloc::plan_and_split(&mut ssa, orig, &ctx));
                        if let (Some(mut plan), Some(body)) = (plan, f.body.as_mut()) {
                            let ps = f.params.as_mut_slice();
                            crate::ssa::regalloc::deliver(&ssa, &mut plan, body, ps);
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
                        home_params: home_param_names(f, home_params_on),
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
                let (dump_on, dump_fn) = rebuild_dump_cfg();
                if *dump_on && dump_fn.as_deref().map(|w| f.name.name == w).unwrap_or(true) {
                    eprintln!(
                        "==== [SSA REBUILD] {} ====\n{:#?}",
                        f.name.name, rebuilt
                    );
                }
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
                        crate::ssa::regalloc::deliver(
                            &ssa,
                            &mut plan,
                            body,
                            f.params.as_mut_slice(),
                        );
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
            BlockItem::Decl(_) => {},
        }
    }
    fn walk_stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a Expr>) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(v) = val {
                    walk_expr(v, out);
                }
            },
            Stmt::Expr { e, .. } => walk_expr(e, out),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                walk_expr(cond, out);
                walk_stmt(then_, out);
                if let Some(x) = else_ {
                    walk_stmt(x, out);
                }
            },
            Stmt::While { cond, body, .. } => {
                walk_expr(cond, out);
                walk_stmt(body, out);
            },
            Stmt::Do { body, cond, .. } => {
                walk_stmt(body, out);
                walk_expr(cond, out);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
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
            },
            Stmt::Block { b, .. } => walk_block(b, out),
            Stmt::Label { stmt, .. } => walk_stmt(stmt, out),
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
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
            },
            Stmt::Invoke { call, .. } => walk_stmt(call, out),
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for o in outputs {
                    walk_expr(o, out);
                }
                for i in inputs {
                    walk_expr(i, out);
                }
            },
            _ => {},
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
            },
            Expr::Binop { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            },
            Expr::Unary { e: x, .. } => walk_expr(x, out),
            Expr::Cast { e: x, .. } => walk_expr(x, out),
            Expr::AddrOf { e: x, .. } => walk_expr(x, out),
            Expr::Deref { e: x, .. } => walk_expr(x, out),
            Expr::Index { arr, idx, .. } => {
                walk_expr(arr, out);
                walk_expr(idx, out);
            },
            Expr::Member { expr, .. } => walk_expr(expr, out),
            Expr::Assign { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            },
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                walk_expr(cond, out);
                walk_expr(then_, out);
                walk_expr(else_, out);
            },
            Expr::Comma { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            },
            Expr::StmtExpr { block, .. } => walk_block(block, out),
            Expr::PostInc { e: x, .. }
            | Expr::PostDec { e: x, .. }
            | Expr::PreInc { e: x, .. }
            | Expr::PreDec { e: x, .. } => walk_expr(x, out),
            _ => {},
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
                    let sigs: Vec<String> = args.iter().map(|a| dbg_short_arg(a)).collect();
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
pub(crate) fn memdbg_rss_kb() -> u64 {
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
pub(crate) fn func_refs_volatile(
    f: &FuncDecl,
    volatile_globals: &std::collections::HashSet<String>,
) -> bool {
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
        // `prelude` 也是体的一部分：`switch (x) { v = *vp; case 1: … }` 里那次 volatile 读
        // 漏了就等于允许这一条语句被当成可移动/可删的参与其他变换。
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            expr_refs_volatile(expr, vol)
                || prelude.iter().any(|i| block_item_refs_volatile(i, vol))
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

/// 已知会「返回两次」(returns_twice) 的函数名。
///
/// **真源不在这里**：本文件的影响域、内联闸门与 `frame_guard` 的护栏必须读同一张表
/// （方案 §4.7「不许另写一份」），所以这里是 `frame_guard::RETURNS_TWICE_NAMES` 的
/// 局部别名。那张表的注释里写清了为什么 libc 的 `longjmp` 一族**不能**进来。
///
/// glibc 头在 `__GNUC__ >= 11` 时会给它们打 `__attribute__((__returns_twice__))`,
/// 但这是最后的兜底: 任何头都不保证标记 (dcc 也刚补齐 `__returns_twice__` 拼写),
/// 且自举 / 其它 libc / 交叉头都不会。因此这里按名字全局识别, 与声明上的
/// `returns_twice` 属性取并集, 保证无论头怎么说, SSA 管线都能正确识别。
const KNOWN_RETURNS_TWICE: &[&str] = crate::frame_guard::RETURNS_TWICE_NAMES;

/// 函数体是否调用了任一 returns_twice 函数。
///
/// `pub(crate)` 的理由：`frame_guard` 的「本函数自己有没有二次返回点」必须与影响域
/// 用同一个遍历器，否则两边对「什么叫调用到 setjmp」会给出不同答案。
pub(crate) fn func_contains_rt_call(
    f: &FuncDecl,
    rt_names: &std::collections::HashSet<String>,
) -> bool {
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
        BlockItem::Decl(Decl::Var(v)) => v
            .init
            .as_ref()
            .is_some_and(|e| expr_contains_rt_call(e, rt_names)),
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
                || else_
                    .as_ref()
                    .is_some_and(|e| stmt_contains_rt_call(e, rt_names))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_contains_rt_call(cond, rt_names) || stmt_contains_rt_call(body, rt_names)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref()
                .is_some_and(|i| stmt_contains_rt_call(i, rt_names))
                || cond
                    .as_ref()
                    .is_some_and(|c| expr_contains_rt_call(c, rt_names))
                || step
                    .as_ref()
                    .is_some_and(|s| expr_contains_rt_call(s, rt_names))
                || stmt_contains_rt_call(body, rt_names)
        },
        Stmt::Block { b, .. } => block_contains_rt_call(b, rt_names),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            expr_contains_rt_call(expr, rt_names)
                || prelude
                    .iter()
                    .any(|i| block_item_contains_rt_call(i, rt_names))
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
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
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
            _ => {},
        }
    }
}

fn collect_callees_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => collect_callees_expr(e, out),
        Stmt::If {
            cond, then_, else_, ..
        } => {
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
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
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
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            collect_callees_expr(expr, out);
            // prelude 里的调用点也是调用点：漏了就与 `ssa::passes::walk_items_refs` 那次
            // 同一病灶（只被 prelude 调用的 static 函数被当作不可达，而 `call` 照样发射）。
            for i in prelude {
                collect_callees_block_item(i, out);
            }
            for c in cases {
                for i in &c.body.items {
                    collect_callees_block_item(i, out);
                }
            }
        },
        Stmt::Invoke { call, .. } => collect_callees_stmt(call, out),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            collect_callees_expr(object, out);
            collect_callees_expr(selector, out);
        },
        _ => {},
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
        _ => {},
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
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
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
        _ => {},
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
    check_items_for_const_stores(&block.items, const_globals)
}

/// 项列表层面。`switch` 除了 `cases` 还有一份 `prelude`（第一个 `case` 之前的语句），
/// 它们不是 `Block`，所以这一层要能单独收。漏它等于「写进 `const` 全局但不报」。
fn check_items_for_const_stores(
    items: &[BlockItem],
    const_globals: &std::collections::HashSet<String>,
) {
    for item in items {
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
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            check_expr_for_const_stores(expr, const_globals);
            check_items_for_const_stores(prelude, const_globals);
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
    items_have_unsafe_ssa_pattern(&b.items, allow_static_locals)
}

/// 项列表层面（`switch` 的 `prelude` 不是 `Block`，但同样可能带 unsafe 形态）。
fn items_have_unsafe_ssa_pattern(items: &[BlockItem], allow_static_locals: bool) -> bool {
    items.iter().any(|item| match item {
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
        //
        // **但本条判据只能盖住有体的嵌套函数定义，不能盖住块作用域的原型声明**
        // （`body == None`）。原型不产生任何代码，重建丢掉它不会丢语义：调用目标靠
        // `referenced_reloc_symbols`（重定位里出现的名字）建 UND 条目，而
        // `__attribute__((error/warning/deprecated))` 与 `__compiletime_error` 的诊断扫描
        // 只看 `prog.decls` 顶层那一条（`codegen.rs:1340-1371`），块内原型**本来就不参与**。
        // 一律当 unsafe 的代价极大：内核陷阱宏的固有形状是
        //     do { extern void __compiletime_assert_N(void) __compiletime_error(msg);
        //          if (!(cond)) __compiletime_assert_N(); } while (0)
        // （`include/linux/compiler_types.h:670-681`，经 `build_bug.h:40` 展开），块里多这一条
        // 原型声明就把整函数踢出 SSA 通道，`if (常量)` 再没人折 ⇒ 实测全树 181 处
        // `undefined reference to '__compiletime_assert_NNN'`（外加 `__scoped_seqlock_*` 一族）。
        // 逐形实测（`-O2`，判据=产物里是否还有指向陷阱的重定位）：
        //   原型在文件作用域 ✔ 折 / 原型在同一个块 ✗ / 原型在内层块 ✗ /
        //   原型带 `__attribute__((noreturn))` ✗ / 条件写成 `y == 0` ✗ —— 即与原型的位置和
        //   属性无关，只与「这个函数里有没有块作用域 `Decl::Func`」有关（gcc 六形全折）。
        BlockItem::Decl(Decl::Func(f)) => f.body.is_some(),
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
        Stmt::Switch { cases, prelude, .. } => {
            // prelude 里也能带「 unsafe 形态」（位域 RMW、可取址局部、VLA……），漏了它
            // 这个函数就会进到 SSA 并在重建时错值。
            items_have_unsafe_ssa_pattern(prelude, allow_static_locals)
                || cases
                    .iter()
                    .any(|c| block_has_unsafe_ssa_pattern(&c.body, allow_static_locals))
        },
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

/// 未被引用的 `static` 数据对象的跨函数 DCE。
///
/// 为什么必须有：「配置开关关掉后整条数据链失去引用」在内核里是常态，而 gcc 从不
/// 发射无人引用的 static 数据。实测现场 `drivers/md/dm.c`：`dm_dax_ops` 只由
/// `alloc_dax(md, &dm_dax_ops)` 引用，而 `CONFIG_DAX=n` 时 `alloc_dax` 是忽略参数的
/// static inline 桩 ⇒ gcc 把 `dm_dax_ops` 连同它初始化器引用的三个 static 函数
/// （`dm_dax_direct_access`/`zero_page_range`/`recovery_write`）一起删。dcc 保留了这个
/// 对象，三个函数随之保活，函数体里的 `dax_get_private(...)` 就成为链接期的未定义
/// 引用（`CONFIG_DAX=n` 时全树无定义）。
///
/// 判据只删「本文件内定义、`static`、且名字在任何保留实体里都没出现过」的对象；
/// 保守方向一律选择留：
///   * 非 static / `extern`（可能有外部引用）;
///   * `used`/`retain`/带 `section`（链接脚本、`__start_`/`__stop_`、gc-sections 语义）；
///   * `alias_name`/`copy_fn`（符号别名的目标）；
///   * 任何 asm 文本里点名过的名字（与函数 DCE 共用 `collect_asm_names`）。
///
/// 引用集从**所有**实体上收（函数体、变量初始化器、文件级 asm 操作数）。只从活实体
/// 上收才能删得干净，但那要函数 DCE 先跑一轮；本轮取「全收」的保守口径：最多是
/// 「只被另一个待删 static 引用的对象也留下」，不会多删。
fn cross_variable_dce(module: &mut Module) {
    let prog = module.source_mut();
    let mut referenced: HashSet<String> = collect_asm_names(prog);
    let collect = |e: &Expr, out: &mut HashSet<String>| {
        if let Expr::Ident { name, .. } = e {
            out.insert(name.name.clone());
        }
    };
    for d in &prog.decls {
        match d {
            Decl::Func(f) => {
                if let Some(b) = &f.body {
                    walk_block_ref(b, &mut |e| collect(e, &mut referenced));
                }
            },
            Decl::Var(v) => {
                if let Some(init) = &v.init {
                    walk_expr_ref(init, &mut |e| collect(e, &mut referenced));
                }
            },
            Decl::GlobalAsm { outputs, inputs, .. } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    walk_expr_ref(e, &mut |e| collect(e, &mut referenced));
                }
            },
            _ => {},
        }
    }
    prog.decls.retain(|d| match d {
        Decl::Var(v) => {
            if !v.is_static || v.is_extern {
                return true;
            }
            if v.is_used_attr
                || v.is_retain
                || v.section.is_some()
                || v.alias_name.is_some()
                || v.copy_fn.is_some()
                || v.asm_label.is_some()
                || v.is_weak
            {
                return true;
            }
            referenced.contains(&v.name.name)
        },
        _ => true,
    });
}

fn cross_function_dce(module: &mut Module) {
    let prog = module.source_mut();
    // **文件里任何 asm 文本提到的名字都当根**（函数体内的 + 文件级 `asm(...)`）。
    // `collect_func_refs` 只走 AST 操作数，不看 `asm_str`；不补这一条，被删的函数
    // 会变成链接期的 `undefined reference`，比不删更坏。这里复用同一个取词器
    // [`collect_asm_names`]（与内联表用的是同一份判据，不另升一套）。
    let asm_names = collect_asm_names(prog);
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
    for name in asm_names {
        extra_roots.push(name);
    }
    crate::ssa::passes::cross_function_dce_with_roots(&mut funcs, true, &extra_roots);
    let mut all: Vec<Decl> = others;
    for f in funcs {
        all.push(Decl::Func(f));
    }
    prog.decls = all;
}

/// 丢弃「无人引用」的 `static inline` 函数定义。
///
/// GCC 对 `static` + `inline` 的语义是「定义即内联体」：没有任何 out-of-line
/// 引用（调用点全部被内联展开、地址没被取、asm 文本没点名）时**不发射**独立
/// 副本 —— `-O0` 实测同样如此（未使用的普通 `static` 会发射，`static inline`
/// 不会：`nm` 里前者有 `t`、后者一个符号都没有）。
///
/// dcc 的 `cross_function_dce` 只在 `level >= 2` 跑，`-O0`/`-O1` 于是把副本
/// 照样发射；而副本体里的 `"i"(形参)`（内核 `__always_inline` 的
/// `arch_static_branch` 这类）离开调用点的常量实参根本折不出立即数 ⇒ 整个 TU
/// 硬错误。实测默认档位：analyzer/torture/asm-x86-linux-cpuid-paravirt-2.c、
/// asm-x86-linux-wfx_get_ps_timeout-full.c。
///
/// 判据刻意保守：从**所有**实体上收名字（函数体、全局初始化器、文件级 asm、
/// asm 文本），只删「除自己体里的引用外没人提到」的 `static inline` 定义。
/// 自引用（递归）是唯一放不下的形状：那点保守只会多留一份体，不会误删。
fn drop_unused_static_inline_defs(module: &mut Module) {
    let prog = module.source_mut();
    let mut referenced: HashSet<String> = collect_asm_names(prog);
    let collect = |e: &Expr, out: &mut HashSet<String>| {
        if let Expr::Ident { name, .. } = e {
            out.insert(name.name.clone());
        }
    };
    for d in &prog.decls {
        match d {
            Decl::Func(f) => {
                // alias/ifunc/copy 的目标是按**名字**引用的，asm 文本取词器看不见。
                if let Some(t) = &f.alias_name {
                    referenced.insert(t.clone());
                }
                if let Some(t) = &f.ifunc_resolver {
                    referenced.insert(t.clone());
                }
                if let Some(b) = &f.body {
                    walk_block_ref(b, &mut |e| collect(e, &mut referenced));
                }
            },
            Decl::Var(v) => {
                if let Some(init) = &v.init {
                    walk_expr_ref(init, &mut |e| collect(e, &mut referenced));
                }
            },
            Decl::GlobalAsm { outputs, inputs, .. } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    walk_expr_ref(e, &mut |e| collect(e, &mut referenced));
                }
            },
            _ => {},
        }
    }
    prog.decls.retain(|d| match d {
        Decl::Func(f) => {
            if !f.is_static || !(f.is_inline || f.is_always_inline) || f.body.is_none() {
                return true;
            }
            // 硬根（used/retain/ctor/dtor/非 inline 的 section/alias/ifunc/…）
            // 与 weak 一律保留：链接期还有别人可能引用它们。
            if f.is_weak || crate::ssa::passes::is_dce_root_hard(f) {
                return true;
            }
            referenced.contains(&f.name.name)
        },
        _ => true,
    });
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
        // 标签包着的语句是一等公民：内联展开器会把整个 callee 体再套一层
        // `__inlN__l__inlN__end:` 标签（`rewrite_returns_to_dest` 的出口），
        // 以前 `_ => {}` 把它整棵子树一起跳过，于是标签里的三元/常量分支
        // 一个都不折。实测 `kernel/sched/build_utility.c` 的 `__wait_on_bit`：
        // `fold_constants` 只折了另外两份副本，标签里那份留到 codegen ⇒ E0311。
        Stmt::Label { stmt, .. } => fold_stmt(stmt, fenv_access),
        // 选择子与每个 case 体：`fold_stmt` 以前完全不进 `switch`（同理，`_ => {}`）。
        // 只折子节点，不在这里选臂（选臂是 `fold_branches_stmt` 的活）。
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            *expr = fold_expr(expr.clone(), fenv_access);
            fold_block(prelude, fenv_access);
            for c in cases.iter_mut() {
                if let Some(v) = &mut c.val {
                    *v = fold_expr(v.clone(), fenv_access);
                }
                if let Some(v) = &mut c.range_end {
                    *v = fold_expr(v.clone(), fenv_access);
                }
                fold_block(&mut c.body.items, fenv_access);
            }
        },
        // asm goto 的输入操作数里就坐着 `"i"(常量表达式)`。
        Stmt::AsmGoto {
            inputs, outputs, ..
        } => {
            for e in inputs.iter_mut().chain(outputs.iter_mut()) {
                *e = fold_expr(e.clone(), fenv_access);
            }
        },
        Stmt::GotoIndirect { expr, .. } => {
            **expr = fold_expr((**expr).clone(), fenv_access);
        },
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
        Stmt::Invoke { call, .. } => fold_stmt(call, fenv_access),
        _ => {},
    }
}

/// `fold_expr` 在初始化器里的臂：逐项折。
fn fold_init_items(items: Vec<crate::ast::InitItem>, fenv_access: bool) -> Vec<crate::ast::InitItem> {
    items
        .into_iter()
        .map(|it| match it {
            crate::ast::InitItem::Expr(x) => crate::ast::InitItem::Expr(fold_expr(x, fenv_access)),
            crate::ast::InitItem::Designated { designator, value } => {
                crate::ast::InitItem::Designated {
                    designator,
                    value: fold_expr(value, fenv_access),
                }
            },
        })
        .collect()
}

/// 只看「值」求常量，允许穿过**可丢的**逗号左操作数。
///
/// `Comma{l, r}` 的值只看 `r`；能否穿过去取决于 `l` 的求值能不能丢，判据只有一个出处：
/// [`crate::semantics::discarded_value_needs_code`]（volatile 读、调用、除法陷入、汇编
/// 都算「不能丢」）。这个函数只回答值，**不代表可以整条不发** —— 要不要保留 `l` 的求值
/// 由调用方按同一个判据决定。
fn const_eval_through_comma(e: &Expr) -> Option<i128> {
    // 0) 还挂着**未解析**的 `__builtin_constant_p` ⇒ 这条表达式此刻没有「值」。
    //    `const_eval` 对它是「实参此刻是不是字面量」，不是「这个局部已被证成常量」：
    //    `static int probe(int n) { return __builtin_constant_p(n); }` 在这里会被答成 0，
    //    而 `fold_const_return_calls` 拿这个 0 当替体就把所有 `probe(…)` 折成 0（实测
    //    `builtin_constant_p_of_inline_temp_still_true` 从 1 变 0）。真正的答案只能由
    //    `ipa_modref` 用 proven 局部表给（它跑在前面，折完这里就没有调用可问了）——
    //    与 [`expr_has_unresolved_constp`] 头注里 `arch/x86/.../amd.c` 那个 0 是同一条坑。
    if expr_has_unresolved_constp(e) {
        return None;
    }
    // 先过「不带可观察效果」这一关，理由有两层：
    //   ① `const_eval::eval_full` 对 `Expr::Assign` 走的是**宽松口径**（返回右值，
    //      副作用留给运行期）——那是给全局初始化器用的，在值语境里它不是常量：
    //      `(*p = 5) != 0` 会被算成常量 1，于是 `a != 0 || (*p = 5) != 0` 整式被吸成 1、
    //      赋值连同 store 一起消失（实测 lor_branch_order 夹具 v 停在 -1）。
    //   ② 逗号左操作数同样不能有副作用，而 `discarded_value_needs_code` 对 `Comma`
    //      两侧都递归，所以这一条就顺带把它盖住了。
    //
    // 第二个闸门只看「编译期折叠型内建」：`discarded_value_needs_code` 对**任何** `Call`
    // 都答「要代码」，于是 `__builtin_expect(0, 0)`（内核 `unlikely()` 展开）把整条
    // `&&` 的定值吸收拦下 —— 实测 `drivers/md/dm.c` 的
    // `static_branch_unlikely(&zoned_enabled) && unlikely(bdev_is_zoned(…))`
    // （`CONFIG_BLK_DEV_ZONED=n` 时右操作数是 0）折不掉，死臂里的 `dm_zone_endio()`
    // 留在产物里。`dropping_value_is_safe` 就是为这种「纯内建调用可以整条不发」
    // 准备的：它只放行 `const_eval` 能算出常量、且实参各自也安全的直接调用。
    if crate::semantics::discarded_value_needs_code(e)
        && !crate::semantics::dropping_value_is_safe(e)
    {
        return None;
    }
    if let Expr::Comma { r, .. } = e {
        return const_eval_through_comma(r);
    }
    const_eval::eval(e).ok()
}

/// `e` 在**不求值**「短路掉的那一侧」的前提下是否定值；答 `Some(v)` 时那个值就是运行期
/// 的值，而且被跳过的部分在 C 语义里本来就不会被执行。
///
/// 与 [`const_eval_through_comma`] 的唯一区别就在这一句：后者先把整个表达式过一遍
/// `discarded_value_needs_code`（对 volatile 读一律答「要代码」），而短路侧里坐着的
/// volatile 读**不会真的发生**。内核现场（`kernel/sched/fair.c` 的
/// `is_rd_overutilized()`）：`return !sched_energy_enabled() || READ_ONCE(rd->overutilized);`
/// 里 `READ_ONCE` 展开的 `*(const volatile unsigned int *)&…` 就是这种「永不执行但
/// 类型带 volatile」的右操作数，按旧口径整条判不成常量。
///
/// 左操作数仍按最严口径（[`const_eval_through_comma`]）——它**总是**要被求值的，
/// 带副作用就必须留住。
fn const_value_short_circuit(e: &Expr) -> Option<i128> {
    match e {
        Expr::Binop {
            op: crate::ast::BinOp::OrOr,
            l,
            r,
            ..
        } => {
            if const_value_short_circuit(l)? != 0 {
                return Some(1);
            }
            Some((const_value_short_circuit(r)? != 0) as i128)
        },
        Expr::Binop {
            op: crate::ast::BinOp::AndAnd,
            l,
            r,
            ..
        } => {
            if const_value_short_circuit(l)? == 0 {
                return Some(0);
            }
            Some((const_value_short_circuit(r)? != 0) as i128)
        },
        // `?:` 同一条规则：没选中的那一支不求值（C11 6.5.15p4）。
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            let c = const_value_short_circuit(cond)?;
            const_value_short_circuit(if c != 0 { then_ } else { else_ })
        },
        _ => const_eval_through_comma(e),
    }
}

/// `&&`/`||` 的「右操作数已定值」吸收：整式恒为 `v`，`l` 的求值按需保成逗号左操作数。
///
/// `l` 在 `&&`/`||` 里**总是**被求值（短路只发生在右操作数上），所以只有它确实不带可观察
/// 效果时才能整体丢掉；否则必须留成 `(l, v)` 这种形状，不能省成裸常量。
fn and_or_result(l: Expr, v: i128, span: Span, ty: crate::ast::Type) -> Expr {
    if crate::semantics::discarded_value_needs_code(&l) {
        Expr::Comma {
            l: Box::new(l),
            r: Box::new(Expr::Int {
                v,
                span: span.clone(),
                ty: ty.clone(),
            }),
            span,
            ty,
        }
    } else {
        Expr::Int { v, span, ty }
    }
}

/// 条件/选择子里是否还挂着**未解析**的 `__builtin_constant_p(...)` 调用。
///
/// 为什么要在折分支与三元之前问这一句：`ipa_modref`（-O2）会把函数体里每个
/// `__builtin_constant_p` 折成 `Int`，而它的答案靠「这个局部是否已证常量」的跨过程事实。
/// 在那之前，凡是用 `const_eval` 折条件的地方只能得到 0 —— `const_eval` 只答得了「实参
/// 此刻是不是字面量」，答不了「内联出来的临时量是不是已证常量」。把这个 0 当定论折下去
/// 就再也回不来：
///
///   实测 `arch/x86/kernel/cpu/mce/amd.c` 的 `min(u32, 0xFFF)`，`__types_ok` 依赖
///   `__is_nonneg(uy) = (__builtin_constant_p((long long)(uy) >= 0) && …)`；内联展开后
///   `prune_spliced_items` 立刻折分支，constp 被答成 0 ⇒ `!__types_ok` 判恒真 ⇒
///   `BUILD_BUG_ON_MSG` 里的 `if` 被换成**无条件** `__compiletime_assert_505()`，之后
///   `ipa_modref` 就算把 constp 证成 1 也无从改起（`if` 已经不在了）。
///   二阶夹具 `.work_tmp/minmax_typesok_twice.c`：gcc -O2 产物 0 条陷阱重定位，dcc 1 条；
///   同一 TU 里只出现一份时（`minmax_typesok.c`）反而是好的 —— 只有「内联拼接」那条路
///   会踩到它，这正是本条只挡「未解析」而不是「永远不折」的理由。
///
/// 判据不设开关：`ipa_modref` 之后函数体里已经没有 constp 调用，这条自动失效，
/// 所以「延后」不会把折叠永久丢掉。
fn expr_has_unresolved_constp(e: &Expr) -> bool {
    match e {
        Expr::Call { callee, args, .. } => {
            if matches!(callee.as_ref(), Expr::Ident { name, .. } if name.name == "__builtin_constant_p")
            {
                return true;
            }
            expr_has_unresolved_constp(callee) || args.iter().any(expr_has_unresolved_constp)
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_has_unresolved_constp(l) || expr_has_unresolved_constp(r)
        },
        Expr::Unary { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::Cast { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::Member { expr: e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::VaArg { ap: e, .. } => expr_has_unresolved_constp(e),
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            expr_has_unresolved_constp(cond)
                || expr_has_unresolved_constp(then_)
                || expr_has_unresolved_constp(else_)
        },
        Expr::Index { arr, idx, .. } => {
            expr_has_unresolved_constp(arr) || expr_has_unresolved_constp(idx)
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            inits.iter().any(|it| match it {
                InitItem::Expr(x) => expr_has_unresolved_constp(x),
                InitItem::Designated { value, .. } => expr_has_unresolved_constp(value),
            })
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            outputs.iter().any(expr_has_unresolved_constp)
                || inputs.iter().any(expr_has_unresolved_constp)
        },
        // 语句表达式：`min()/max()` 把整条 `BUILD_BUG_ON` 装在这里面，不能不看。
        Expr::StmtExpr { block, .. } => block.items.iter().any(item_has_unresolved_constp),
        _ => false,
    }
}

fn item_has_unresolved_constp(it: &BlockItem) -> bool {
    match it {
        BlockItem::Stmt(s) => stmt_has_unresolved_constp(s),
        BlockItem::Decl(Decl::Var(v)) => v
            .init
            .as_ref()
            .map(expr_has_unresolved_constp)
            .unwrap_or(false),
        BlockItem::Decl(Decl::Func(f)) => f
            .body
            .as_ref()
            .map(|b| b.items.iter().any(item_has_unresolved_constp))
            .unwrap_or(false),
        _ => false,
    }
}

fn stmt_has_unresolved_constp(s: &Stmt) -> bool {
    match s {
        Stmt::Expr { e, .. } => expr_has_unresolved_constp(e),
        Stmt::Return { val, .. } => val.as_ref().map(expr_has_unresolved_constp).unwrap_or(false),
        Stmt::If {
            cond,
            then_,
            else_,
            ..
        } => {
            expr_has_unresolved_constp(cond)
                || stmt_has_unresolved_constp(then_)
                || else_
                    .as_ref()
                    .map(|e| stmt_has_unresolved_constp(e))
                    .unwrap_or(false)
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            expr_has_unresolved_constp(cond) || stmt_has_unresolved_constp(body)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref()
                .map(|i| stmt_has_unresolved_constp(i))
                .unwrap_or(false)
                || cond
                    .as_ref()
                    .map(expr_has_unresolved_constp)
                    .unwrap_or(false)
                || step
                    .as_ref()
                    .map(expr_has_unresolved_constp)
                    .unwrap_or(false)
                || stmt_has_unresolved_constp(body)
        },
        Stmt::Block { b, .. } => b.items.iter().any(item_has_unresolved_constp),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            expr_has_unresolved_constp(expr)
                || prelude.iter().any(item_has_unresolved_constp)
                || cases
                    .iter()
                    .any(|c| c.body.items.iter().any(item_has_unresolved_constp))
        },
        Stmt::Label { stmt, .. } => stmt_has_unresolved_constp(stmt),
        _ => false,
    }
}

fn fold_expr(e: Expr, fenv_access: bool) -> Expr {
    let e = match e {
        Expr::Binop { op, l, r, span, ty } => {
            let l = Box::new(fold_expr(*l, fenv_access));
            let r = Box::new(fold_expr(*r, fenv_access));
            // 短路求值：`0 && R` 恒为 0、`1 || R` 恒为 1，而 **R 根本不会被求值** ⇒ R 里有
            // 函数调用也照样能丢。这一条不是顺手优化：内核里 `RCU_LOCKDEP_WARN(...)` 展开就
            // 是 `do { } while (0 && (!(lockdep_is_held(&l->m) || rcu_read_lock_held())));`，
            // 而 `lockdep_is_held`/`lock_is_held` 在 `!CONFIG_PROVE_LOCKING` 下只有 `extern`
            // 声明（`include/linux/lockdep.h` 注释自己写「depend on dead code elimination」），
            // 不丢 R 就变成链接期 `undefined reference to 'lockdep_is_held'`（实测 94 + 17 处）。
            // 反面：选择子 L 非常量时绝不能动 R（会改变到底调不调用）。
            match op {
                crate::ast::BinOp::AndAnd => {
                    if let Expr::Int { v, .. } = l.as_ref() {
                        if *v == 0 {
                            return Expr::Int { v: 0, span, ty };
                        }
                    }
                    // 【`X && 常量0` 的吸收】`l` 求值、`r` 定值：`r` 的值恒 0 时整式恒 0。
                    //
                    // 为什么不能只靠上面的「左零」：常量桩代回把实参求值写成
                    // `Comma{实参, 常量}`（见 `fold_const_return_calls`），值在逗号右边，
                    // 而 `const_eval::eval` 要求逗号**左边也是常量** ⇒ 整条 `&&` 链折不下来。
                    // 实测 `mm/memory.o`：
                    //     else if (details && details->single_folio
                    //              && folio_test_pmd_mappable(details->single_folio)  // THP 关 ⇒ return false 桩
                    //              && next - addr == HPAGE_PMD_SIZE && pmd_none(*pmd))
                    // 死臂里的 `__compiletime_assert_521` 就卡在这（gcc 那一侧整条链是常量假）。
                    if const_eval_through_comma(&r) == Some(0) {
                        return and_or_result(*l, 0, span, ty);
                    }
                },
                crate::ast::BinOp::OrOr => {
                    if let Expr::Int { v, .. } = l.as_ref() {
                        if *v != 0 {
                            return Expr::Int { v: 1, span, ty };
                        }
                    }
                    // 对称的一条：`r` 的值恒非 0 ⇒ 整式恒 1（`l` 的求值照保）。
                    if let Some(v) = const_eval_through_comma(&r) {
                        if v != 0 {
                            return and_or_result(*l, 1, span, ty);
                        }
                    }
                },
                _ => {},
            }
            // 【一侧是 0 的位运算恒等式】`const_eval` 只在**两侧都能求值**时才给答案，
            // 而内核里大量这一形：
            //     ia32_frame &= (IS_ENABLED(CONFIG_X86_32) || IS_ENABLED(CONFIG_IA32_EMULATION));
            // 右半边折成字面量 0，整条却留在运行时，于是紧随其后的
            // `if (ia32_frame && use_fxsr())` 也折不掉；死支里只有 CONFIG_IA32_EMULATION
            // 才存在的 `convert_from_fxsr`/`convert_to_fxsr` 于是成了链接期 undefined
            // reference（实测 `arch/x86/kernel/fpu/signal.o`：各一条，gcc 那一侧整块消失）。
            // 只在「被吸收的那一侧不需要发代码」时做：`f(&x) & 0` 仍必须调用 f，
            // volatile 读也仍必须读（用 `discarded_value_needs_code` 而不是
            // `expr_has_side_effects`：后者对 volatile 读和 StmtExpr 都不敏感）。
            if ty.is_int() {
                let zero = |e: &Expr| matches!(e, Expr::Int { v: 0, .. });
                // `x & 0` / `x * 0` ⇒ 0
                if matches!(op, crate::ast::BinOp::And | crate::ast::BinOp::Mul) {
                    let other = if zero(&l) {
                        Some(&r)
                    } else if zero(&r) {
                        Some(&l)
                    } else {
                        None
                    };
                    if let Some(o) = other {
                        if !crate::semantics::discarded_value_needs_code(o) {
                            return Expr::Int { v: 0, span, ty };
                        }
                    }
                }
                // `x | 0` / `x ^ 0` / `x << 0` / `x >> 0` ⇒ x（结果类型一致时才收，
                // 否则整型提升/转换的类型得留着）
                if zero(&r)
                    && matches!(
                        op,
                        crate::ast::BinOp::Or
                            | crate::ast::BinOp::Xor
                            | crate::ast::BinOp::Shl
                            | crate::ast::BinOp::Shr
                    )
                    && l.ty() == ty
                {
                    return *l;
                }
                // 交换律那一形：`0 | x` / `0 ^ x`。parser 的复合赋值展开把可交换
                // 运算的 RHS 摆在左边（`x |= 0` ⇒ `Binop{ Or, l: 0, r: x }`，
                // 让 RHS 先求值），所以恒等式必须两侧都试。
                if zero(&l)
                    && matches!(op, crate::ast::BinOp::Or | crate::ast::BinOp::Xor)
                    && r.ty() == ty
                {
                    return *r;
                }
            }
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
                    // LongDouble / Float128 不做编译期折叠：宿主无 80-bit /
                    // 128-bit 运算，f64 中转会把 binary128 的低位舍掉——
                    // `1.0f128 - FLT128_EPSILON * 0.5f128` 的正确值
                    // 1 - 2^-113 在 f64 里就是 1.0，整条比较因此恒真
                    // (float128-floath.c 最后一条检查 abort)。
                    if lc.is_long_double()
                        || rc.is_long_double()
                        || lc.is_float128()
                        || rc.is_float128()
                    {
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
                    // 常量传播成常量后这里应折 1 (修复 modref-2)；若没有 SCCP
                    // (如 -O0) 则保留调用，由后端兜底 (变量参数 → 0)。
                    if const_eval::is_constant_p_arg(&args[0]) {
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
        // 三元 `?:`：先把三个子表达式各自折叠，**再**判条件常量。
        // 缺这一步就不是「少一个优化」而是「语义留住运行时」：内核
        // `static_cpu_has(bit)`/`cpu_feature_enabled(bit)` 展开就是
        // `(__builtin_constant_p(...) && REQUIRED_MASK_BIT_SET(...)) ? 1 : _static_cpu_has(bit)`，
        // 条件原本不是 `Int`，只有子表达式折完才变成 `Int`。条件不折，整个三元留在
        // 运行时，里面的 `__always_inline` 调用点就永远嵌在表达式里不被展开
        // ⇒ `try_static_inline_general` 的 `call_site_shape` 不接这一形，callee 另发一份
        // 独立副本，而它自己的 `"i"` 操作数只能是寄存器（硬失败）。
        Expr::Cond {
            cond,
            then_,
            else_,
            span,
            ty,
        } => {
            let cond = Box::new(fold_expr(*cond, fenv_access));
            let then_ = Box::new(fold_expr(*then_, fenv_access));
            let else_ = Box::new(fold_expr(*else_, fenv_access));
            // 条件到底是不是编译期值：先认「折完就是 `Int`」，再走 `const_eval` 整体求值。
            // 只认 `Int` 会漏掉一整类条件：`__builtin_constant_p(内联临时量)` 在 parser 与
            // 上面那条 `Call` 臂里都是「不急切折 0」（要把机会留给 SCCP/modref），于是它
            // 永远不是 `Int`，三元就留在运行时 ⇒ 两个分支都被编，而非常量的那一支里带
            // `"i"(形参)` 的 asm 根本编不出（实测 `kernel/ptrace.o`：`include/linux/wait_bit.h`
            // 的 `wait_on_bit` 展开就是 `__builtin_constant_p(bit) ? 常量写法 : 运行时写法`，
            // 报 `impossible constraint in 'asm' … (1 << (__inl3151$_nr & 7))`）。
            // 判据不另起一份：`fold_branches_block` 折 `Stmt::If` 用的就是同一条 `const_eval`，
            // 而 `__builtin_constant_p` 的答案也由 `const_eval` 自己在同一个地方给。
            // 条件里还挂着未解析的 `__builtin_constant_p` 时**不折**这一条：`const_eval`
            // 此刻只能答 0，折下去就选错边（现场见 [`expr_has_unresolved_constp`]）。
            let taken_is_then = match cond.as_ref() {
                Expr::Int { v, .. } => Some(*v != 0),
                _ if expr_has_unresolved_constp(&cond) => None,
                _ => const_eval::eval_full(&cond, fenv_access)
                    .ok()
                    .and_then(|v| v.as_int())
                    .map(|v| v != 0),
            };
            if let Some(taken_then) = taken_is_then {
                // 【死臂里定义了源标号就不能折】`1 ? 1 : ({ a : 1; })` 被丢的是 else 臂，
                // 而 `a` 的定义就在那个语句表达式里，外面的 `&&a` 仍指向它 ⇒ 发射期
                // `E1103: references internal label '.Ll_f_a' which has no entry in this
                // TU's symtab`（`tests/gcc-torture` 的 `compile/pr17913.c`）。
                // 判据与语句级拦网同一条口径：非内联器自造的标号一律保守留住
                // （[`is_inliner_label`]）；`&&label` 不进 `live_refs`，所以这里
                // 不区分「臂外真有没有人引用」。
                let dropped: &Expr = if taken_then { &else_ } else { &then_ };
                let dropped_has_src_label = crate::ast_dispatch::label_defs_of_expr(dropped)
                    .iter()
                    .any(|d| !is_inliner_label(d));
                if !dropped_has_src_label {
                    let taken = if taken_then { then_ } else { else_ };
                    // 结果类型还是原三元的 `ty`：C 的 `?:` 已做过通常算术转换，直接搬
                    // 分支会把那次转换弄丢（`c ? 5 : 7.5` 这类的 `ty` 是 double，
                    // 取 `5` 那一支时必须按 `ty` 转，不能留成 int 的 5）。
                    let taken_ty = taken.ty();
                    let e = if taken_ty == ty {
                        taken
                    } else {
                        Box::new(Expr::Cast {
                            ty: ty.clone(),
                            e: taken,
                            span: span.clone(),
                        })
                    };
                    // 【条件求值不能随分支一起丢】`?:` 的条件**总是先求值一次**
                    // （C11 6.5.15p4）：条件本身带副作用（赋值/自增/调用）时，把三元
                    // 折成选中分支等于把那次求值删掉。实测 `int y = (x = 0) ? 1 : 2;`
                    // 在 -O1/-O2 让 x 停在 1（-O0 正常）。做法与 [`and_or_result`]
                    // 同源：留成 `(cond, taken)`，值只看右侧。
                    let e = if crate::semantics::discarded_value_needs_code(&cond) {
                        Box::new(Expr::Comma {
                            l: cond,
                            r: e,
                            span: span.clone(),
                            ty: ty.clone(),
                        })
                    } else {
                        e
                    };
                    return fold_expr(*e, fenv_access);
                }
            }
            Expr::Cond {
                cond,
                then_,
                else_,
                span,
                ty,
            }
        },
        // 赋值右值与逗号两侧都要下探。
        //
        // 这两形以前整个落在 `other => other` 里，而**内联展开的产物就是它们**：
        // `__inlN$rv += (条件 ? A : B)`、`__inlN$rv += ({ … })`。以前不暴露是因为
        // `inline_expr` 每层自己会调 `fold_expr`；一旦某一次折叠被「条件里挂着未解析的
        // `__builtin_constant_p` 就延后」这条推开（见 [`expr_has_unresolved_constp`]），
        // 它就**必须**有第二次机会 —— `fold_constants` 跑在 `ipa_modref` 之后，constp
        // 此时已经是 `Int`，正好接得住。没有这一支，延后的三元会永远留在运行时：
        // integration 用例 `builtin_constant_p_ternary_condition_is_decided_at_compile_time`
        // 当场变红（`use_rt` 里 `ctba` 的 `"i"(1 << (__inl6$_nr & 7))` 被编出来 ⇒ E0311）。
        // 两侧都不搬、只折子节点，所以不会把赋值表达式折成一个值（`const_eval` 对
        // `Assign` 的宽松口径仍然只能用在「值语境」的调用方，如 `const_eval_through_comma`）。
        Expr::Assign {
            op,
            l,
            r,
            span,
            ty,
        } => {
            let r = Box::new(fold_expr(*r, fenv_access));
            // 【`x op= 0` 的恒等式】复合赋值不走 [`fold_expr`] 的 `Binop` 臂（AST 里它是
            // `Assign` 带 `op`），所以上面那套「一侧是 0」的化简接不住它。内核现场：
            //     ia32_frame &= (IS_ENABLED(CONFIG_X86_32) || IS_ENABLED(CONFIG_IA32_EMULATION));
            // 右边是字面量 0，整条却留在运行时 ⇒ `if (ia32_frame && use_fxsr())` 折不掉
            // ⇒ 死支里 `convert_from_fxsr`/`convert_to_fxsr` 变成链接期 undefined
            // reference（`arch/x86/kernel/fpu/signal.o`）。
            // `&=`/`*=` ⇒ 退化成 `x = 0`（交给 fwd 的常量存储表）；`|=`/`^=`/移位的
            // 0 是恒等 ⇒ 整个赋值退化成一次读。左值带 volatile 时不动（写是可观察效果）。
            if !crate::ast::type_has_volatile(&l.ty()) && l.ty().is_int() {
                if let Expr::Int { v: 0, .. } = r.as_ref() {
                    match op {
                        // 普通 `=` 在 AST 里也是 `Assign`，`op` 用 `Add` 当哨兵
                        // （判据与 `store_const_stmt` 同源），所以不会撞进下面两臂。
                        crate::ast::BinOp::And | crate::ast::BinOp::Mul => {
                            return Expr::Assign {
                                op: crate::ast::BinOp::Add,
                                l,
                                r: Box::new(Expr::Int {
                                    v: 0,
                                    span: span.clone(),
                                    ty: ty.clone(),
                                }),
                                span,
                                ty,
                            };
                        },
                        crate::ast::BinOp::Or
                        | crate::ast::BinOp::Xor
                        | crate::ast::BinOp::Shl
                        | crate::ast::BinOp::Shr
                            if l.ty() == ty =>
                        {
                            return *l;
                        },
                        _ => {},
                    }
                }
            }
            // 【自赋值削掉】上面两条恒等式把 `x |= 0` 这类复合赋值退化成 `x = x`。
            // 留着它没有语义问题，但 `propagate_const_stores_forward` 会把它当成
            // 「给 x 存了一个非恒量」而把 x 从常量表里清掉 —— 于是紧随其后的
            // `if (!x)` 又折不动了。只在两侧是同一个裸名字、且无 volatile 时削掉。
            if !crate::ast::type_has_volatile(&l.ty()) {
                if let (Expr::Ident { name: ln, .. }, Expr::Ident { name: rn, .. }) =
                    (l.as_ref(), r.as_ref())
                {
                    if ln.name == rn.name && l.ty() == r.ty() {
                        return *l;
                    }
                }
            }
            Expr::Assign {
                op,
                l,
                r,
                span,
                ty,
            }
        },
        // 【纯左操作数削掉】`(l, r)` 里 `l` 的值本来就被丢弃，所以它只要**没有可观察效果**
        // 就可以整条省成 `r` —— 判据与 [`and_or_result`] 造逗号时用的是同一个
        // [`discarded_value_needs_code`]（一侧造、一侧削，方向必须互补，不能各定一套）。
        //
        // 为什么必须由折叠层来做而不是 `const_eval` 的 Comma 臂：那一臂的 `eval(l)?` 服务的是
        // 「C 常量表达式」语境（数组维度、初始化器），那里 `(x, 0)` 按标准确实**不是**常量，
        // 放开会改变 `__builtin_constant_p` 一类判据的答案。而运行期代码里的逗号是普通表达式。
        //
        // 现场（`mm/swap.o` 的 `free_zone_device_folio`、`mm/gup.o` 的
        // `migrate_device_coherent_folio`）：`CONFIG_ZONE_DEVICE=n` 时
        // `memdesc_is_zone_device(mdf) { return 0; }` 被常量桩代回，桩的实参求值留在逗号左侧，
        // 于是条件长成
        //     __builtin_expect(!!({ folio->flags 那次纯读 }, (Bool)0), 0)
        // `__builtin_expect` 这一层已经会透传，但透传进去的 `!!(Comma)` 因为 `eval(l)` 失败而
        // 整条判不成常量 ⇒ 死分支里的调用照发 ⇒ 那函数只在 `#ifdef CONFIG_ZONE_DEVICE` 里有
        // 定义 ⇒ 链接期 undefined reference。
        Expr::Comma { l, r, span, ty } => {
            let l = Box::new(fold_expr(*l, fenv_access));
            let r = Box::new(fold_expr(*r, fenv_access));
            if crate::semantics::discarded_value_needs_code(&l) {
                Expr::Comma { l, r, span, ty }
            } else {
                *r
            }
        },
        // 【offsetof 的下标】`offsetof(struct blob, arr[i][ilog2(m)])` 在 kernel 里是
        // 「运行时偏移计算」（`indices` 就是为此保留的），但折叠器以前不进这些下标 ⇒
        // 下标里的表达式带着未折的三元被原样发射。现场（实测
        // `drivers/gpu/drm/i915/gt/uc/intel_guc_ads.o` 的
        // `intel_guc_engine_usage_record_map`）：
        //     offsetof(struct __guc_ads_blob,
        //              engine_usage.engines[guc_class][ilog2(engine->logical_mask)])
        // 而 `ilog2()` 是（`include/linux/log2.h:156`）
        //     __builtin_constant_p(n) ? … : (sizeof(n) <= 4) ? __ilog2_u32(n) : __ilog2_u64(n)
        // 不折的结果是两条臂都发出去，产物里同时出现 `__ilog2_u32` 与 `__ilog2_u64`
        // 两个引用——它们在头里是 `static __always_inline`，gcc 全部内联掉，
        // 一个外部引用都不留。
        Expr::Offsetof {
            ty, member, indices, span, result_ty,
        } => {
            let folded: Vec<Box<Expr>> = indices
                .into_iter()
                .map(|ix| Box::new(fold_expr(*ix, fenv_access)))
                .collect();
            Expr::Offsetof { ty, member, indices: folded, span, result_ty }
        },
        // **语句表达式整块就地折**。`fold_expr` 以前没有这一臂，落进 `other => other`，
        // 而 `fold_stmt` 又靠 `fold_expr` 才能进入表达式位置上的块 —— 于是所有
        // **内联展开出来的 `({ … })` 体里一个常量都没折过**：
        // 实测 `kernel/sched/build_utility.c` 的 `__wait_on_bit`：
        // `__inlN$rv += (__builtin_constant_p(len) ? ({ "i"(1 << (len & 7)) 那条 asm }) : ({ 运行时 asm }))`
        // 套在 `({ ... if (...) { ... } })` 里；`ipa_modref` 已把 constp 折成 `Int(0)`，
        // 但 `fold_constants` 走不进这个 StmtExpr ⇒ 三元一直不选边 ⇒ 死支里那条
        // `"i"` 操作数被当成活代码编，报
        // `E0311: … (expression: (1 << (__inl8226$_nr & 7)), in function __wait_on_bit)`。
        // 逐项折不动形状（语句表达式的值仍是最后一个表达式），所以原地折是等价的。
        Expr::StmtExpr {
            block: mut b,
            span,
            ty,
        } => {
            fold_block(&mut b.items, fenv_access);
            Expr::StmtExpr {
                block: b,
                span,
                ty,
            }
        },
        // 初始化器里的常量表达式同样从来没折过（`{ .a = 1 + 2 }` 在内核 asm 模板
        // 的偏移量里到处都是）。同上面：只搬子节点，不改形状。
        Expr::CompoundLit {
            ty: cty,
            inits,
            span,
        } => Expr::CompoundLit {
            ty: cty,
            inits: fold_init_items(inits, fenv_access),
            span,
        },
        Expr::InitList { inits, span, ty } => Expr::InitList {
            inits: fold_init_items(inits, fenv_access),
            span,
            ty,
        },
        other => other,
    };
    // 【语句表达式的整体回填】下面那条通用折叠只在 `e` **已经是字面量**时才生效，于是
    // 「原本不是字面量、但可求值」的节点永远折不掉。内核 `__always_inline` 助手被搬进
    // 调用方后正长成一个语句表达式：`field_mask(0xffff)` 求得出 0xffff，却因为节点形状
    // 不是 Int 而原样留在树里 ⇒ 外层 `if (__builtin_constant_p(v) && (v & ~field_mask(f)))`
    // 判不成常量 ⇒ 死支里那个「只有声明没有定义」的 trap 调用发进 `.text` ⇒
    // `LD .tmp_vmlinux1` 报 undefined reference（实测 localmodconfig 全树
    // `__field_overflow` 21 / `__bad_mask` 8 / `__bad_udelay` 3 条，同配置 gcc 一条没有）。
    //
    // 只对语句表达式开这个口子：[`const_eval`] 的解释器已经把「写外层变量」判成拒绝，
    // 所以求值成功的语句表达式必然没有逃逸的副作用，换成常量是等价的。其余形状
    // （含 Assign / ++-- / 真函数调用）走原来的臂，不在这里动。
    if let Expr::StmtExpr { span, ty, .. } = &e {
        if let Ok(crate::const_eval::ConstValue::Int(i)) = const_eval::eval_full(&e, fenv_access) {
            return Expr::Int {
                v: i,
                span: span.clone(),
                ty: ty.clone(),
            };
        }
    }
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
    // 文件作用域的可观察对象（volatile / _Atomic / 段限定）先把名字收齐。
    // 为什么只能按**名字**收：限定符记在 `VarDecl::quals`/`addr_space` 上，而
    // `Expr::Ident` 只带名字与裸类型 —— 拿着表达式永远看不出「这条读是不是 volatile」。
    // 发射器侧试过同一个判据（`emit/expr/cast.rs` 的 void 臂），结果把
    // `(void)(vglob)` 这一读误删了（实测：对 volatile 全局的引用从 1 条变成 0 条）。
    let global_observable: std::collections::HashSet<String> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Var(v) if crate::ast::var_decl_access_is_observable(v) => Some(v.name.name.clone()),
            _ => None,
        })
        .collect();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                // 本函数的可观察局部：整棵树扫一遍再传下去。名字级并集是**保守方向**：
                // 内层同名遮蔽只会让该删的多留一条，不会让不该删的被删。
                let mut observable = global_observable.clone();
                collect_observable_names_items(&body.items, &mut observable);
                prune_block(&mut body.items, &observable);
            }
        }
    }
}

/// 死局部声明的删除：名字在整个函数体里**一次都没被表达式提到**、且初始化式求值没有
/// 副作用的局部声明，整条删掉（连同它在帧里的那一格）。
///
/// 为什么必须删（报告 §5.39）：帧槽在规划期按**声明**分配
/// （`func_setup::pre_register_vars`），而 [`prune_dead_stores`] 已经把死写削掉了 —— 于是
/// 槽留着、读写一条也没有。实测 `__schedule` 的变量区 77 槽里只有 19 个偏移被真正触及；
/// 同一函数 gcc 的帧是 96 B，dcc 是 832 B。
///
/// 判据为什么是「整函数体一个引用都没有」而不是「本作用域内没有引用」：名字解析要看
/// 作用域链，而按**全函数**收名字只会多留（外层同名会让内层那条也不删），不会把某个引用
/// 解析到另一个对象上。
///
/// 两条已知的漏判（方向都是「仍然占槽」，不是错值）：
///   * 语句表达式 `({ int x = …; })` 里的声明：本 pass 不穿进表达式（那里没有可用的
///     安全递归），所以那一族一条都不删；
///   * 只经 `x = …` 赋值、从不被读的写法：赋值左值会作为 `Expr::Ident` 进 `used` 集合。
fn prune_dead_locals(module: &mut Module) {
    let dbg = std::env::var_os("DCC_DEAD_DECL_DUMP").is_some();
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let Some(body) = &mut f.body else { continue };
            // 名字按**整函数体**收一次：这是本 pass 唯一的语义输入。
            let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
            walk_block_ref(body, &mut |e| {
                if let Expr::Ident { name, .. } = e {
                    used.insert(name.name.clone());
                }
            });
            // 类型里的变量引用也要收：见 [`collect_vla_dim_names_items`] 的注释。
            collect_vla_dim_names_items(&body.items, &mut used);
            let before = count_local_decls(&body.items);
            prune_dead_local_items(&mut body.items, &used);
            let after = count_local_decls(&body.items);
            if dbg && before != after {
                eprintln!("DEAD_DECL {} {before} -> {after}", f.name.name);
            }
        }
    }
}

/// 把**类型里**的变量引用也收进 `out`。
///
/// VLA 的维度表达式住在 `Type::Array(_, ArraySize::Var(e))` 里，不是语句树上的
/// `Expr`
/// —— [`walk_block_ref`] 看不到它。而 `fold_constants` / `propagate_*` 会把体内对那个
/// 名字的读全部代回常量，于是「整函数体一次引用都没有」成立、声明被 [`prune_dead_locals`]
/// 删掉，而 VLA 在运行期仍要读那个名字来算尺寸：实测
/// `int n = 4; int a[n];` 的 `n` 被删后编出 `E1134: frame slot for 'n' was never
/// planned`（夹具 `cold_outline_blocked_by_vla_in_body`）。
fn collect_vla_dim_names_items(items: &[BlockItem], out: &mut std::collections::HashSet<String>) {
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => collect_vla_dim_names_ty(&v.ty, out),
            BlockItem::Stmt(s) => collect_vla_dim_names_stmt(s, out),
            _ => {},
        }
    }
}

fn collect_vla_dim_names_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::Block { b, .. } => collect_vla_dim_names_items(&b.items, out),
        Stmt::If { then_, else_, .. } => {
            collect_vla_dim_names_stmt(then_, out);
            if let Some(e) = else_ {
                collect_vla_dim_names_stmt(e, out);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => collect_vla_dim_names_stmt(body, out),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_vla_dim_names_stmt(i, out);
            }
            collect_vla_dim_names_stmt(body, out);
        },
        Stmt::Switch {
            cases, prelude, ..
        } => {
            collect_vla_dim_names_items(prelude, out);
            for c in cases {
                collect_vla_dim_names_items(&c.body.items, out);
            }
        },
        Stmt::Label { stmt, .. } => collect_vla_dim_names_stmt(stmt, out),
        Stmt::Invoke { call, .. } => collect_vla_dim_names_stmt(call, out),
        _ => {},
    }
}

/// 类型里所有 VLA 维度表达式提到的名字。
fn collect_vla_dim_names_ty(ty: &Type, out: &mut std::collections::HashSet<String>) {
    match ty {
        Type::Array(elem, sz) => {
            if let crate::ast::ArraySize::Var(e) = sz {
                walk_expr_ref(e, &mut |x| {
                    if let Expr::Ident { name, .. } = x {
                        out.insert(name.name.clone());
                    }
                });
            }
            collect_vla_dim_names_ty(&elem.base, out);
        },
        Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
            for f in fields.iter() {
                collect_vla_dim_names_ty(&f.ty, out);
            }
        },
        _ => {},
    }
}

/// 一棵语句树里还有多少个局部声明（只用于 `DCC_DEAD_DECL_DUMP` 的读数）。
fn count_local_decls(items: &[BlockItem]) -> usize {
    let mut n = 0usize;
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(_)) => n += 1,
            BlockItem::Stmt(s) => n += count_local_decls_stmt(s),
            _ => {},
        }
    }
    n
}

fn count_local_decls_stmt(s: &Stmt) -> usize {
    match s {
        Stmt::Block { b, .. } => count_local_decls(&b.items),
        Stmt::If { then_, else_, .. } => {
            count_local_decls_stmt(then_)
                + else_.as_ref().map_or(0, |e| count_local_decls_stmt(e))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => count_local_decls_stmt(body),
        Stmt::For { init, body, .. } => {
            init.as_ref().map_or(0, |i| count_local_decls_stmt(i)) + count_local_decls_stmt(body)
        },
        Stmt::Switch { cases, prelude, .. } => {
            count_local_decls(prelude)
                + cases
                    .iter()
                    .map(|c| count_local_decls(&c.body.items))
                    .sum::<usize>()
        },
        Stmt::Label { stmt, .. } => count_local_decls_stmt(stmt),
        Stmt::Invoke { call, .. } => count_local_decls_stmt(call),
        _ => 0,
    }
}

/// 初始化式求值有没有副作用。`InitItem` 的两种形状都要下去：`int a[2] = {f(), 1}` 里的
/// `f()` 是必须保留的副作用，把它当纯的就会把整条声明删掉、调用消失。
fn decl_init_is_pure(e: &Expr) -> bool {
    match e {
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            inits.iter().all(|it| match it {
                crate::ast::InitItem::Expr(x) => decl_init_is_pure(x),
                crate::ast::InitItem::Designated { value, .. } => decl_init_is_pure(value),
            })
        },
        _ => !crate::semantics::expr_has_side_effects(e),
    }
}

/// 这一条声明能不能整条删。
fn dead_local_decl(v: &VarDecl, used: &std::collections::HashSet<String>) -> bool {
    // 有静态存储期/可观察读写的声明一律不动：它们不占帧，删了还会改语义。
    if v.is_static || v.is_extern || v.quals.is_volatile() || v.cleanup_fn.is_some() {
        return false;
    }
    // 已经绑了家的（`register asm` 与 SSA 交付的寄存器家）不动：删声明会把
    // 「序言保存哪个号」与绑定表拆开。
    if v.register_name.is_some() || v.home.is_some() {
        return false;
    }
    if used.contains(&v.name.name) {
        return false;
    }
    match &v.init {
        None => true,
        Some(e) => decl_init_is_pure(e),
    }
}

fn prune_dead_local_items(items: &mut Vec<BlockItem>, used: &std::collections::HashSet<String>) {
    items.retain(|it| !matches!(it, BlockItem::Decl(Decl::Var(v)) if dead_local_decl(v, used)));
    for it in items.iter_mut() {
        // 只走语句：声明里的初始化式（可能含语句表达式块）与嵌套函数的体留给别的 pass。
        if let BlockItem::Stmt(s) = it {
            prune_dead_local_stmt(s, used);
        }
    }
}

fn prune_dead_local_stmt(s: &mut Stmt, used: &std::collections::HashSet<String>) {
    match s {
        Stmt::Block { b, .. } => prune_dead_local_items(&mut b.items, used),
        Stmt::If { then_, else_, .. } => {
            prune_dead_local_stmt(then_, used);
            if let Some(e) = else_ {
                prune_dead_local_stmt(e, used);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => prune_dead_local_stmt(body, used),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prune_dead_local_stmt(i, used);
            }
            prune_dead_local_stmt(body, used);
        },
        // switch 的 `prelude`（第一个 case 之前的声明）与每个 case 体都是一个块。
        Stmt::Switch {
            cases, prelude, ..
        } => {
            prune_dead_local_items(prelude, used);
            for c in cases {
                prune_dead_local_items(&mut c.body.items, used);
            }
        },
        Stmt::Label { stmt, .. } => prune_dead_local_stmt(stmt, used),
        Stmt::Invoke { call, .. } => prune_dead_local_stmt(call, used),
        _ => {},
    }
}

/// 扫描一片语句/声明，把「每次访问都可观察」的名字并进 `out`（含嵌套块与嵌套函数体）。
fn collect_observable_names_items(items: &[BlockItem], out: &mut std::collections::HashSet<String>) {
    for item in items {
        match item {
            BlockItem::Decl(d) => collect_observable_names_decl(d, out),
            BlockItem::Stmt(s) => collect_observable_names_stmt(s, out),
            _ => {},
        }
    }
}

fn collect_observable_names_decl(d: &Decl, out: &mut std::collections::HashSet<String>) {
    match d {
        Decl::Var(v) => {
            if crate::ast::var_decl_access_is_observable(v) {
                out.insert(v.name.name.clone());
            }
        },
        // 嵌套函数的体不在调用方的语句树里，得递进去收；它的局部名与外层同名时
        // 只会多留不少留，方向安全。
        Decl::Func(f) => {
            if let Some(body) = &f.body {
                collect_observable_names_items(&body.items, out);
            }
        },
        _ => {},
    }
}

fn collect_observable_names_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::Block { b, .. } => collect_observable_names_items(&b.items, out),
        Stmt::If { then_, else_, .. } => {
            collect_observable_names_stmt(then_, out);
            if let Some(e) = else_ {
                collect_observable_names_stmt(e, out);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => collect_observable_names_stmt(body, out),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_observable_names_stmt(i, out);
            }
            collect_observable_names_stmt(body, out);
        },
        Stmt::Switch { cases, prelude, .. } => {
            // switch 的两堆语句体都得扫：`prelude`（第一个 case 之前的声明/标签）与
            // 每条 case 自己的 `Block`。
            collect_observable_names_items(prelude, out);
            for c in cases {
                collect_observable_names_items(&c.body.items, out);
            }
        },
        _ => {},
    }
}

/// 一条「值被丢弃」的表达式语句（裸 `e;` 与 `(void)e;` 是同一个语境）能不能整条删掉。
///
/// 两道闸都要过：
///   1. `semantics::discarded_value_needs_code` —— 结构性黑名单（只承认「结构上确定
///      碰不到世界」的形状），它处理 call/赋值/volatile **类型**的读/会陷入的除模；
///   2. 本函数 —— 结构上过了再按**声明**问一遍：根名字里只要有一个是可观察对象
///      （volatile / _Atomic / 段限定），就不删。这一道是 1. 看不见的（表达式不带限定符）。
///
/// 本规则要拿下的现场（内核全树未定义引用之一）：
/// `include/linux/compiler-context-analysis.h:242` 的 `token_context_lock(RCU, …)` 只发
/// `extern const struct __ctx_lock_RCU *RCU;`，注释明写「not backed by a real data
/// structure (**linker error if accidentally referenced**)」—— 这类名字天生没有定义；
/// 而 `rcupdate.h:456` 把 `lockdep_assert_in_rcu_read_lock()` 定成
/// `do { (void)(RCU); } while (0)`。gcc 一个字节都不发，dcc 以前发 `mov RCU(%rip),%rax`
/// ⇒ 值虽丢但**重定位留下**，最终链接报 `undefined reference to 'RCU'`（实测
/// `io_uring/tw.o`）。与 GAS 口径一致的另一面：没有重定位就没有 UND 条目。
fn discarded_statement_is_dead(e: &Expr, observable: &std::collections::HashSet<String>) -> bool {
    if crate::semantics::discarded_value_needs_code(e) {
        return false;
    }
    let mut names = Vec::new();
    discarded_read_root_names(e, &mut names);
    names.iter().all(|n| !observable.contains(n))
}

/// 收出「纯读」表达式碰到的根名字。只走 `discarded_value_needs_code` 判为「不需发码」
/// 的那几个臂；认不得的形态什么都不收（不删）——但那种情况在上面已被拦下，这里
/// 的默认臂只负责「字面量类没有名字」。
fn discarded_read_root_names(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Ident { name, .. } => out.push(name.name.clone()),
        Expr::Cast { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Unary { e, .. }
        | Expr::Deref { e, .. } => discarded_read_root_names(e, out),
        Expr::Member { expr, .. } => discarded_read_root_names(expr, out),
        Expr::Index { arr, idx, .. } => {
            discarded_read_root_names(arr, out);
            discarded_read_root_names(idx, out);
        },
        Expr::Binop { l, r, .. } => {
            discarded_read_root_names(l, out);
            discarded_read_root_names(r, out);
        },
        Expr::Cond { cond, then_, else_, .. } => {
            discarded_read_root_names(cond, out);
            discarded_read_root_names(then_, out);
            discarded_read_root_names(else_, out);
        },
        Expr::Comma { l, r, .. } => {
            discarded_read_root_names(l, out);
            discarded_read_root_names(r, out);
        },
        _ => {},
    }
}

fn prune_block(items: &mut Vec<BlockItem>, observable: &std::collections::HashSet<String>) {
    items.retain(
        |item| !matches!(item, BlockItem::Stmt(Stmt::Expr { e, .. }) if discarded_statement_is_dead(e, observable)),
    );
    for item in items.iter_mut() {
        match item {
            BlockItem::Stmt(s) => prune_stmt(s, observable),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &mut f.body {
                    let mut inner = observable.clone();
                    collect_observable_names_items(&body.items, &mut inner);
                    prune_block(&mut body.items, &inner);
                }
            },
            _ => {},
        }
    }
}

fn prune_stmt(s: &mut Stmt, observable: &std::collections::HashSet<String>) {
    match s {
        Stmt::If { then_, else_, .. } => {
            prune_stmt(then_, observable);
            if let Some(e) = else_ {
                prune_stmt(e, observable);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => prune_stmt(body, observable),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                prune_stmt(i, observable);
            }
            prune_stmt(body, observable);
        },
        Stmt::Block { b, .. } => prune_block(&mut b.items, observable),
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
                // 先把「只写过一次、初值能折成常量」的局部代回读取点，再折分支。
                //
                // 为什么必须先代回：强制内联拼接会把形参实体化成 `__inlN$_x` 临时量，
                // 于是内核那一族尺寸检查宏在拼好的代码里长成
                //   `int __inl0$_size = (int)sizeof(*p); switch (__inl0$_size) { … }`
                // ——选择子是 `Ident` 而不是 `Int`，常量 switch 折叠根本看不到它（实测
                // `DCC_FOLD_TRACE` 打的就是 `bail=selector-not-const sel=Ident(__inl0$_size)`）。
                // gcc 靠早期内联后的常量传播消除这一步；dcc 里这一段复用与
                // `prune_spliced_items`/`fold_asm_immediate_consts` **同一个**候选判定与
                // 代回器（只写一次 + 初值可常量折叠 + 作用域内 + 被写过就标脏），
                // 不另升第二套规则。
                let mut cands: HashMap<String, Expr> = HashMap::new();
                let mut dirty = std::collections::HashSet::new();
                subst_cand_reads_items(&mut body.items, &mut cands, &mut dirty);
                // 死分支折叠要问「丢掉的臂里的标号，臂外还有没有人跳」（`asm goto` 的
                // `%l[...]` 落点也算引用）。名字缺失就是 E1103，所以这里预先收全。
                let live_refs: std::collections::HashSet<String> =
                    crate::ast_dispatch::collect(body).goto_targets.into_iter().collect();
                fold_branches_block(&mut body.items, fenv_access, &live_refs);
            }
        }
    }
}

/// 语句（递归）内是否含**可能被外部 `goto` 跳进来**的标号。
///
/// goto 可跳入常量死分支中的标签（C 里标号是函数级作用域），折叠时若丢弃标签会导致
/// codegen 报 `unresolved label`。内联器自造的标号例外，理由见 [`is_inliner_label`]：
/// 那里必须用**同一个**谓词，不能各写一份文法 —— 实测教训：本函数原先用只认
/// `__inl<N>__end`（及其嵌套形）的 `is_inline_exit_label`，于是被 `__inl<N>__l` 前缀
/// 改名的**源标签 / `asm goto` 落点**（`__inl5__ll_yes`）不算自造标号，拦网白白触发：
/// `include/linux/jump_label.h` 的 `static_branch_unlikely()` 里 `arch_static_branch_jump()`
/// 一被内联，死臂里就带上了这个标号，`branch = ____wrong_branch_error()` 那条恒假支
/// 永远塌不掉（实测 `drivers/tty/serial/8250/8250_platform.o`、`net/ipv4/route.o` 等 10 个
/// 对象留 `undefined reference to ____wrong_branch_error`，而 gcc 一个都不留）。
fn stmt_has_label(s: &Stmt) -> bool {
    match s {
        Stmt::Label { name, stmt, .. } => {
            !is_inliner_label(&name.name) || stmt_has_label(stmt)
        },
        Stmt::Block { b, .. } => block_has_label(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_label(then_) || else_.as_ref().map(|e| stmt_has_label(e)).unwrap_or(false)
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            stmt_has_label(body)
        },
        Stmt::Switch { cases, prelude, .. } => {
            items_have_label(prelude) || cases.iter().any(|c| block_has_label(&c.body))
        },
        _ => false,
    }
}

/// 项列表层面（`switch` 的 `prelude` 里也能有标号：`switch (x) { out: case 1: … }`）。
fn items_have_label(items: &[BlockItem]) -> bool {
    items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_label(s),
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().map(block_has_label).unwrap_or(false),
        _ => false,
    })
}

fn block_has_label(b: &Block) -> bool {
    b.items.iter().any(|item| match item {
        BlockItem::Stmt(s) => stmt_has_label(s),
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().map(block_has_label).unwrap_or(false),
        _ => false,
    })
}

/// 「这条语句里定义了**任意**标号（含内联自造的出口标号）」—— [`stmt_has_label`] 的
/// 不豁免版。给两条「按位置整段删项」的机制用：本文件的不可达截除与 `dce_block`/`dce_stmt`。
///
/// 方向与 `stmt_has_label` 相反：那里「认不准就当没标号」只会少折一个死分支（不破坏正确性，
/// 因为删掉的臂带着自己的落点一起消失）；这里「认不准就当有标号」才安全，删错了是把一个
/// 还有 `goto` 指向的定义弄丢，当场编译失败（E1102）而不是默默生成错码。
fn stmt_has_any_label(s: &Stmt) -> bool {
    match s {
        Stmt::Label { .. } => true,
        Stmt::Block { b, .. } => block_has_any_label(b),
        Stmt::If {
            cond,
            then_,
            else_,
            ..
        } => {
            expr_has_any_label(cond)
                || stmt_has_any_label(then_)
                || else_.as_ref().map(|e| stmt_has_any_label(e)).unwrap_or(false)
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_has_any_label(cond) || stmt_has_any_label(body)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().map(|i| stmt_has_any_label(i)).unwrap_or(false)
                || cond.as_ref().map(|c| expr_has_any_label(c)).unwrap_or(false)
                || step.as_ref().map(|s| expr_has_any_label(s)).unwrap_or(false)
                || stmt_has_any_label(body)
        },
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            expr_has_any_label(expr)
                || prelude.iter().any(|i| item_has_any_label(i))
                || cases.iter().any(|c| block_has_any_label(&c.body))
        },
        Stmt::Expr { e, .. } => expr_has_any_label(e),
        Stmt::Return { val, .. } => val.as_ref().map(|e| expr_has_any_label(e)).unwrap_or(false),
        // 跳转本身不定义标号，删掉只会少一条引用。
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Goto { .. } => false,
        // `asm goto` 的跳出目标、`&&label` 取到的落点、C++ 的 invoke/landingpad：
        // 本函数不拆它们里面的引用，一律当「这里有标号相关的事」不肯删。
        _ => true,
    }
}

fn block_has_any_label(b: &Block) -> bool {
    b.items.iter().any(item_has_any_label)
}

fn item_has_any_label(item: &BlockItem) -> bool {
    match item {
        BlockItem::Stmt(s) => stmt_has_any_label(s),
        // 块里声明的变量不定义标号；嵌套函数定义（clang 扩展）里可能有，宁可当有。
        BlockItem::Decl(Decl::Func(_)) => true,
        BlockItem::Decl(_) => false,
    }
}

/// 表达式里是否藏着「能定义标号的语句」。C 里表达式只能是值的计算，而 GCC 语句表达式
/// `({ … })` 可以携带整块语句，那里面就能写 `out: …` —— 所以本函数唯一真正的入口是
/// `Expr::StmtExpr`，其余形状必须递归下去看（调用实参里就能藏 `({...})`）。
///
/// 为什么不写成「一律 false」：那会把 `({ a: ; })` 这种定义弄丢，同样是一条悬空跳转。
/// 为什么不写成「一律 true」：那会把每一条普通函数调用都当成有标号，截除与死代码清理
/// 一起失效（实测本族要删的就是 `rv = (item == 3 || item == 9);` 这类普通赋值/表达式语句）。
fn expr_has_any_label(e: &Expr) -> bool {
    fn inits_has(items: &[crate::ast::InitItem]) -> bool {
        items.iter().any(|it| match it {
            crate::ast::InitItem::Expr(x) => expr_has_any_label(x),
            crate::ast::InitItem::Designated {
                value,
                designator: _,
            } => expr_has_any_label(value),
        })
    }
    match e {
        // 字面量、名字、类型形的 `sizeof`/`_Alignof`/`offsetof`、`&&label`：都不带块。
        // `sizeof(expr)`/`_Alignof(expr)` 的操作数按 C 不被求值，也不会有人把它当入口。
        Expr::Int { .. }
        | Expr::Char { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Ident { .. }
        | Expr::LabelAddr { .. }
        | Expr::SizeofTy { .. }
        | Expr::SizeofExpr { .. }
        | Expr::AlignofTy { .. }
        | Expr::AlignofExpr { .. }
        | Expr::VaArgPack { .. } => false,
        Expr::StmtExpr { block, .. } => block_has_any_label(block),
        Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::Cast { e, .. }
        | Expr::Unary { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. } => expr_has_any_label(e),
        Expr::Member { expr, .. } => expr_has_any_label(expr),
        Expr::VaArg { ap, .. } => expr_has_any_label(ap),
        Expr::Offsetof { indices, .. } => {
            indices.iter().any(|i| expr_has_any_label(i))
        },
        Expr::Index { arr, idx, .. } => {
            expr_has_any_label(arr) || expr_has_any_label(idx)
        },
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. } => expr_has_any_label(l) || expr_has_any_label(r),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_has_any_label(cond)
                || expr_has_any_label(then_)
                || expr_has_any_label(else_)
        },
        Expr::Call { callee, args, .. } => {
            expr_has_any_label(callee) || args.iter().any(expr_has_any_label)
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => inits_has(inits),
        Expr::AsmExpr { outputs, inputs, .. } => {
            outputs.iter().any(expr_has_any_label) || inputs.iter().any(expr_has_any_label)
        },
    }
}

/// `if (({ …; TAIL }))` 的真值：只认「块内某个常量局部原样当尾值」这一类。
///
/// 为什么需要它：`WARN_ON()` / `BUG_ON()` 一族都是
/// ```text
/// ({ int __ret_warn_on = !!(c);
///    if (unlikely(__ret_warn_on)) __WARN();      ← 体里坐着 `asm goto` 的 ud2
///    unlikely(__ret_warn_on); })
/// ```
/// 整块常量求值器过不去（中间那条 `if` 折不出来），而 `subst_cprop` 的契约只覆盖
/// 条件与选择子、**不代回**普通表达式位置，于是尾值里的 `__ret_warn_on` 一直是 Ident。
/// gcc 靠 SSA 常量传播把初值代进尾值再选边。dcc 缺这一步的后果是「整片后继代码
/// 不可达」判不出来：实测 `drivers/iommu/iommu.o` 的 `pci_device_group()` 里
/// `if (WARN_ON(!dev_is_pci(dev)))`（CONFIG_PCI=n ⇒ `dev_is_pci()` 是 `(false)`）
/// 没选边，后面的 `pci_for_each_dma_alias()` / `pci_acs_path_enabled()` 整片保留 ⇒
/// 两条链接期 undefined reference —— 那两个函数的定义在这份 config 下根本没编。
///
/// 安全性：调用方（[`fold_branches_block`]）把条件原样留成一条语句，这里只回答
/// 「哪一臂」。判据要求尾值就是那个常量局部本身（只剥 `!` / 转型 / `__builtin_expect`，
/// 三者都保持真值），并且块内**没有任何语句写它/取它地址/在 asm 里操作它** ——
/// 任一条不满足就答 None，退回原来的保守折法。
fn stmt_expr_branch_truth(e: &Expr) -> Option<bool> {
    let trace = std::env::var_os("DCC_SX_TRACE").is_some();
    macro_rules! bail {
        ($($t:tt)*) => {{
            if trace {
                eprintln!("SX| BAIL {}", format!($($t)*));
            }
            return None;
        }};
    }
    macro_rules! note {
        ($($t:tt)*) => {{
            if trace {
                eprintln!("SX| {}", format!($($t)*));
            }
        }};
    }
    let Expr::StmtExpr { block, .. } = e else {
        bail!("not-stmtexpr");
    };
    let Some((last, init)) = block.items.split_last() else {
        bail!("empty block");
    };
    let BlockItem::Stmt(Stmt::Expr { e: tail, .. }) = last else {
        bail!("tail not expr-stmt");
    };
    let Some((name, negate)) = stmt_expr_tail_bool(tail) else {
        bail!("tail not name");
    };
    note!("name={name} negate={negate} init_items={}", init.len());
    let mut init_val: Option<i128> = None;
    for it in init {
        match it {
            // 同名声明只能是那一条；重复声明说明有作用域把名字遮住了，不判。
            BlockItem::Decl(Decl::Var(v)) if v.name.name == name => {
                if init_val.is_some() {
                    bail!("dup decl {name}");
                }
                let Some(init_expr) = v.init.as_ref() else {
                    bail!("decl no init {name}");
                };
                let Ok(v0) = const_eval::eval(init_expr) else {
                    bail!("decl init not const {name}");
                };
                init_val = Some(v0);
            },
            BlockItem::Stmt(s) => {
                if stmt_writes_local(s, &name) {
                    bail!("written {name}");
                }
            },
            _ => {},
        }
    }
    let Some(v0) = init_val else {
        bail!("no init seen for {name}");
    };
    note!("ok {name}={v0} negate={negate}");
    Some((v0 != 0) ^ negate)
}

/// 尾值剥壳到「一个局部名」：`x`、`!x`、`!!x`、`(int)x`、`__builtin_expect(x, c)`。
/// 返回 `(名字, 是否按真值取反)` —— 这几形都保持 C 的真值语义（`!` 逐层翻转）。
fn stmt_expr_tail_bool(e: &Expr) -> Option<(String, bool)> {
    match e {
        Expr::Ident { name, .. } => Some((name.name.clone(), false)),
        Expr::Unary {
            op: crate::ast::UnOp::Not,
            e: x,
            ..
        } => stmt_expr_tail_bool(x).map(|(n, b)| (n, !b)),
        Expr::Cast { e: x, .. } => stmt_expr_tail_bool(x),
        Expr::Call { callee, args, .. } => {
            let Expr::Ident { name, .. } = callee.as_ref() else {
                return None;
            };
            if name.name != "__builtin_expect" {
                return None;
            }
            stmt_expr_tail_bool(args.first()?)
        },
        _ => None,
    }
}

/// 语句 `s` 会不会写、或者交出局部 `name`：赋值目标 / 自增自减 / `&name` /
/// `asm` 操作数。答 true 只是让 [`stmt_expr_branch_truth`] 放弃，不改变任何求值。
fn stmt_writes_local(s: &Stmt, name: &str) -> bool {
    let mut hit = false;
    walk_stmt_ref(s, &mut |e| match e {
        Expr::Assign { l, .. } => hit |= lvalue_root_is_name(l, name),
        Expr::PreInc { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::PostDec { e, .. } => hit |= lvalue_root_is_name(e, name),
        Expr::AddrOf { e, .. } => hit |= lvalue_root_is_name(e, name),
        // `asm("…" : "+m"(x))` 的操作数就是 `x` 本身（没有 `&`），上面那条拦不住。
        Expr::AsmExpr { outputs, inputs, .. } => {
            hit |= outputs
                .iter()
                .chain(inputs.iter())
                .any(|x| lvalue_root_is_name(x, name));
        },
        _ => {},
    });
    hit
}

/// `e` 当左值时根是不是局部 `name`（剥成员/下标/解引用/转换）。
fn lvalue_root_is_name(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Ident { name: n, .. } => n.name == name,
        Expr::Member { expr, .. } => lvalue_root_is_name(expr, name),
        Expr::Index { arr, .. } => lvalue_root_is_name(arr, name),
        Expr::Deref { e, .. } | Expr::Cast { e, .. } => lvalue_root_is_name(e, name),
        _ => false,
    }
}

/// 丢掉一条死分支时，里面定义、且**臂外还有引用**的标号会不会丢？
///
/// 判据分两半：
///   * 源标签（不带 `__inl<N>__` 前缀）：一律拦阻。C 的标号是函数级作用域，本层
///     看不见函数别处的 `goto`，宁可少折一条死分支。
///   * 内联器改名的标号（`__inl<N>__l…`）：**只有**在分支外还有 `goto`/`asm goto`
///     引用时才拦阻。这一半是 `include/linux/uaccess.h` 的 `unsafe_get_user()` 逼出来的：
///     ```c
///     __label__ local_label;
///     arch_unsafe_get_user(x, ptr, local_label);   /* asm goto(... : : : : local_label) */
///     if (0) { local_label: goto label; }          /* 定义在死块里，引用在活代码里 */
///     ```
///     按 `is_inliner_label` 的旧口径（前缀 ⇒ 自造 ⇒ 可删）会把定义连同死块一起删掉，
///     而 `asm goto` 的落点还在 ⇒ 发射期
///     `E1103: relocation … references internal label '.Ll_<fn>___inl<N>__llocal_label'
///     which has no entry in this TU's symtab`（实测 `arch/x86/kernel/signal_64.c`）。
///     反过来，`jump_label.h` 那族「`asm goto` 与落点同坐一条被丢的臂」必须仍然能删，
///     否则 `____wrong_branch_error` 的恒假支又塌不掉 —— 所以判据是「分支**外**的引用」，
///     不是「有人跳」。
///
/// `live_refs` 是本轮折叠开始时函数级「有 goto 指向」的标号集合
/// （[`crate::ast_dispatch::DispatchFacts::goto_targets`]，含 `asm goto` 的 `%l[...]`
/// 落点）。折叠过程中它只可能变旧（删掉的引用不从集合里消失），变旧只会让我们多留
/// 一条死分支（方向保守），不会造成悬空引用。`&&label` 不在这个集合里：源标签那一半
/// 已经保守拦住了，而内联器改名的标号被 `&&label` 引用的形状在本树里不存在。
fn branch_drop_loses_live_label(s: &Stmt, live_refs: &std::collections::HashSet<String>) -> bool {
    let defs = crate::ast_dispatch::label_defs_of_stmt(s);
    if defs.is_empty() {
        return false;
    }
    let (inner_refs, _) = crate::ast_dispatch::goto_facts_of_stmt(s);
    defs.iter().any(|d| {
        if !is_inliner_label(d) {
            return true;
        }
        live_refs.contains(d) && !inner_refs.iter().any(|r| r == d)
    })
}

fn fold_branches_block(
    items: &mut Vec<BlockItem>,
    fenv_access: bool,
    live_refs: &std::collections::HashSet<String>,
) {
    let mut i = 0;
    while i < items.len() {
        // 【逗号条件的左操作数提句】`if ((l, r)) S` 的值只看 `r`，但 `l` 的求值不能丢。
        //
        // 为什么必须有这一步：`fold_const_return_calls` 把常量桩代回时，**不在纯读白名单里**
        // 的实参要保持求值，方式是拼成 `Comma { l: 实参, r: 常量 }`。于是内核里
        //     if (pmd_is_huge(*pmd)) {                // THP 关闭时 pmd_is_huge() 是 `return 0` 桩
        //         if (next - addr != HPAGE_PMD_SIZE)  // HPAGE_PMD_ORDER = ({ BUILD_BUG_ON(1); 0; })
        // 在代回后成了 `if ((*pmd, 0))`；而 `const_eval::eval` 要求逗号**左边也是常量**
        // ⇒ 判不出这个 0 ⇒ 死臂里那条**无条件**的 `__compiletime_assert_NNN()` 留在产物里
        //（实测 `mm/memory.o` 的 466/467/468/520/521/522 六条，`zap_pmd_range` 的 AST 里
        // 形状就是 `1UL << ({ do { … if (!(!(1))) __compiletime_assert_520(); } while (0); 0; })`）。
        //
        // 拆一层就够：`(a, (b, c))` 右递归，下一轮循环接着拆。
        if let Some(BlockItem::Stmt(Stmt::If { cond, .. })) = items.get_mut(i) {
            if matches!(cond, Expr::Comma { .. }) {
                let item = items.remove(i);
                let BlockItem::Stmt(Stmt::If {
                    cond,
                    then_,
                    else_,
                    span,
                }) = item
                else {
                    unreachable!()
                };
                let Expr::Comma { l, r, .. } = cond else {
                    unreachable!()
                };
                items.insert(
                    i,
                    BlockItem::Stmt(Stmt::Expr {
                        e: *l,
                        span: span.clone(),
                    }),
                );
                items.insert(
                    i + 1,
                    BlockItem::Stmt(Stmt::If {
                        cond: *r,
                        then_,
                        else_,
                        span,
                    }),
                );
                i += 1;
                continue;
            }
        }
        // 【区间真值】条件折不成常量，但**值域**能定案。
        //
        // 现场（`mm/vmstat.o` 2 条 + `fs/proc/task_mmu.o` 1 条 `__compiletime_assert_NNN`）：
        // `BUILD_BUG_ON(SWP_TYPE_SHIFT < SWP_PFN_BITS)` 展开成
        // `if (!(!(58 < (pgtable_l5_enabled() ? 52 : 46) - 12)))`；选择子内联后是带
        // `asm goto` 的语句表达式，常量求值器判不出真值，但两臂的值域 [34,40] 与 58
        // 无交 ⇒ 恒假。GCC 同样是按值域（VRP）消掉的。
        //
        // 与上面常量折法的一处关键差别：**条件本身的求值要留下来当语句**。
        // gcc 编的 `fs/proc/task_mmu.o` 里 `softleaf_has_pfn` 开头那条 `jmp` 就是这段
        // ALT 序列——它保留了 `asm goto`，只删了不可达分支。所以这里同样不能把条件整条
        // 丢掉，否则会削掉 `.altinstructions` 条目（条件经 [`discarded_value_needs_code`]
        // 确认无副作用时才直接丢）。
        let range_if = match items.get(i) {
            Some(BlockItem::Stmt(Stmt::If { cond, .. }))
                if !expr_has_unresolved_constp(cond) && const_eval::eval(cond).is_err() =>
            {
                const_eval::truth_by_int_range(cond)
            },
            _ => None,
        };
        // 【语句表达式尾值】`if (({ …; EXPR }))`：块内语句折不出来（中间常坐着
        // `asm goto`），但 EXPR 自己就是常量 ⇒ 选边确定。判据与安全性见
        // [`stmt_expr_branch_truth`] 的头注；条件求值仍原样保留成一条语句
        // (`discarded_value_needs_code` 对 `StmtExpr` 一律答「需要代码」)。
        let stmt_expr_if = match items.get(i) {
            Some(BlockItem::Stmt(Stmt::If { cond, .. }))
                if !expr_has_unresolved_constp(cond) && const_eval::eval(cond).is_err() =>
            {
                stmt_expr_branch_truth(cond)
            },
            _ => None,
        };
        // 条件里挂着未解析的 `__builtin_constant_p` 时不折这一条 if：`const_eval` 此刻
        // 只能答 0，折下去会把「编译期检查」判成「该报错」（现场见
        // [`expr_has_unresolved_constp`]，`prune_spliced_items` 就在这条路上）。
        let fold_if = match items.get(i) {
            Some(BlockItem::Stmt(Stmt::If { cond, .. })) if !expr_has_unresolved_constp(cond) => {
                const_eval::eval(cond).is_ok()
            },
            _ => false,
        };
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
                // 被丢弃的臂里若定义了「臂外还有人跳」的标号就不能折：判据见
                // [`branch_drop_loses_live_label`]（源标签一律拦，内联前缀标号只看臂外引用）。
                let has_label = if v != 0 {
                    else_
                        .as_ref()
                        .map(|e| branch_drop_loses_live_label(e, live_refs))
                        .unwrap_or(false)
                } else {
                    branch_drop_loses_live_label(&then_, live_refs)
                };
                if has_label {
                    // 分支本身留下来了，但分支**里面**的常量 if/switch 还要继续折：
                    // 现场（`kernel/rseq.o` 的 `__put_user_bad`）里 `switch (sizeof(*p))`
                    // 正坐在 `unsafe_put_user` 自造的 `if (0) { label: … }` 死臂里，
                    // 这里直接 continue 会让那条 switch 永远进不了 `fold_branches_stmt`。
                    let mut then_ = then_;
                    let mut else_ = else_;
                    fold_branches_stmt(&mut then_, fenv_access, live_refs);
                    if let Some(el) = else_.as_mut() {
                        fold_branches_stmt(el, fenv_access, live_refs);
                    }
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
                // 【条件求值不能随未选中臂一起丢】与 `collapse_const_if_operand` /
                // `range_if` 两支同一条做法：`if` 的条件总是先求值一次，它本身需要
                // 发码时（赋值/自增/调用……）先留成一条语句，再接选中臂。
                // 旧版直接只插选中臂，于是 `while ((x = 0));` 这类条件里的 store
                // 被整条删除（实测 `while` / `for` 两支同此）。
                let mut out: Vec<BlockItem> = Vec::new();
                if crate::semantics::discarded_value_needs_code(&cond) {
                    out.push(BlockItem::Stmt(Stmt::Expr {
                        e: cond,
                        span: span.clone(),
                    }));
                }
                match taken {
                    Stmt::Block { b, .. } => out.extend(b.items),
                    other => out.push(BlockItem::Stmt(other)),
                }
                fold_branches_block(&mut out, fenv_access, live_refs);
                for it in out.into_iter().rev() {
                    items.insert(i, it);
                }
                continue;
            }
        } else if let Some(truth) = range_if.or(stmt_expr_if) {
            let item = items.remove(i);
            let BlockItem::Stmt(Stmt::If {
                cond,
                then_,
                else_,
                span,
            }) = item
            else {
                unreachable!()
            };
            // 被丢弃的臂里若定义了「臂外还有人跳」的标号就不能折
            // （与上面常量折法同一条判据）。
            let dropped_has_label = if truth {
                else_
                    .as_ref()
                    .map(|e| branch_drop_loses_live_label(e, live_refs))
                    .unwrap_or(false)
            } else {
                branch_drop_loses_live_label(&then_, live_refs)
            };
            if dropped_has_label {
                // 同上：if 整体保留，但两臂内部仍要下探折叠。
                let mut then_ = then_;
                let mut else_ = else_;
                fold_branches_stmt(&mut then_, fenv_access, live_refs);
                if let Some(el) = else_.as_mut() {
                    fold_branches_stmt(el, fenv_access, live_refs);
                }
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
            let kept = if truth {
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
            let mut out: Vec<BlockItem> = Vec::new();
            // 条件在原文里本来就被求值一次，这里原样保留成一条语句，语义等价。
            if crate::semantics::discarded_value_needs_code(&cond) {
                out.push(BlockItem::Stmt(Stmt::Expr {
                    e: cond,
                    span: span.clone(),
                }));
            }
            match kept {
                Stmt::Block { b, .. } => out.extend(b.items),
                other => out.push(BlockItem::Stmt(other)),
            }
            fold_branches_block(&mut out, fenv_access, live_refs);
            for it in out.into_iter().rev() {
                items.insert(i, it);
            }
            continue;
        } else if let BlockItem::Stmt(s) = &mut items[i] {
            fold_branches_stmt(s, fenv_access, live_refs);
        } else if let BlockItem::Decl(Decl::Var(v)) = &mut items[i] {
            // 声明的初值里同样可能坐着 `({...})`（`int ret = ({ switch …; });` 就是本族宏
            // 最常见的用法之一），不补这一支还是看不到那个 switch。
            if let Some(e) = &mut v.init {
                fold_branches_in_expr(e, fenv_access, live_refs);
            }
        }
        // 【不可达截除】无条件跳转之后、下一个标签之前的项不可达。
        //
        // 为什么必须有这一步：内联把 `return e;` 改写成 `rv = e; goto end;`，而
        // **`return` 的终止性没被带上** —— 源码里其后的语句就以「普通项」的身份紧跟在
        // `goto end;` 后面。`if (IS_ENABLED(X)) return false;`（X 关 ⇒ 条件恒真）折完后是
        // `rv = 0; goto end;` 后跟 `rv = (item == ...);`，于是 `rv` 有两个写入，
        // `if (rv)` 就折不成常量，死块里的 `BUILD_BUG()` 整片留下
        // （实测 `mm/vmstat.o` 的 `__compiletime_assert_483/485`）。
        //
        // 边界：
        //   * 遇到**任何**标号定义就停 —— `goto` 可能从别处跳进来（包括本条 goto 自己的目标：
        //     `goto end; end: …` 那种形状里 end: 后面是活代码）。用的必须是
        //     [`stmt_has_any_label`] 而不是 [`stmt_has_label`]：后者把内联自造的出口标号
        //     （`__inl<N>__l__inl<N>__end`）当透明，那是为「折叠这条臂会不会丢落点」服务的，
        //     那里对（落点和臂一起消失）。截除是按位置删后面一整段，它看不见函数里**别处**的
        //     `goto` —— 实测 `fs/pidfs.o`、`kernel/sched/fair.o`：把 `end:` 的定义删了而
        //     `goto end;` 还在 ⇒ codegen 报 `E1102: internal label … referenced by a jump …
        //     are never defined`，整个 TU 编译失败；
        //   * 遇到声明也停（不碰作用域）。
        let jump_target = match items.get(i) {
            Some(BlockItem::Stmt(Stmt::Goto { target, .. })) => Some(target.name.clone()),
            Some(BlockItem::Stmt(
                Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. },
            )) => Some(String::new()),
            _ => None,
        };
        if let Some(target) = jump_target {
            loop {
                match items.get(i + 1) {
                    Some(BlockItem::Stmt(Stmt::Label { name, .. })) if name.name == target => {
                        break
                    },
                    Some(BlockItem::Stmt(s)) if !stmt_has_any_label(s) => {
                        items.remove(i + 1);
                    },
                    _ => break,
                }
            }
        }
        i += 1;
    }
}

/// 表达式树里下探到所有 `({...})` 块体，对它们跑同一趟分支折叠。
///
/// 为什么需要它：内核那一族尺寸检查宏是 `#define __raw_cmpxchg(ptr, old, new, size, lock)
/// ({ … switch (size) … })`，展开后 switch 坐在**语句表达式**里；而调用点几乎总是把宏的
/// 值再参进更大的表达式（`if (cmpxchg(...) == old)`）或赋给变量，于是只看 `Stmt::Expr`
/// 顶层一个 `StmtExpr` 根本碰不到它（实测 `drivers/tty/tty_ldsem.o` 有 13 条对故意不存在的
/// `__xadd_wrong_size`/`__cmpxchg_wrong_size` 的真引用）。
///
/// 臂表与 `rename_idents_in_expr` 保持同一份形状：两个walkers 一旦覆盖不一致，就会出现
/// 「同一个构造一条路径看得到、另一条看不到」这种最难查的不对称（本轮已付过几次学费）。
fn fold_branches_in_expr(
    e: &mut Expr,
    fenv_access: bool,
    live_refs: &std::collections::HashSet<String>,
) {
    match e {
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } => {
            fold_branches_in_expr(l, fenv_access, live_refs);
            fold_branches_in_expr(r, fenv_access, live_refs);
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
        | Expr::PreDec { e: x, .. } => fold_branches_in_expr(x, fenv_access, live_refs),
        Expr::Call { callee, args, .. } => {
            fold_branches_in_expr(callee, fenv_access, live_refs);
            for a in args {
                fold_branches_in_expr(a, fenv_access, live_refs);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            fold_branches_in_expr(cond, fenv_access, live_refs);
            fold_branches_in_expr(then_, fenv_access, live_refs);
            fold_branches_in_expr(else_, fenv_access, live_refs);
        },
        Expr::Index { arr, idx, .. } => {
            fold_branches_in_expr(arr, fenv_access, live_refs);
            fold_branches_in_expr(idx, fenv_access, live_refs);
        },
        Expr::Member { expr, .. } => fold_branches_in_expr(expr, fenv_access, live_refs),
        Expr::Comma { l, r, .. } => {
            fold_branches_in_expr(l, fenv_access, live_refs);
            fold_branches_in_expr(r, fenv_access, live_refs);
        },
        Expr::VaArg { ap, .. } => fold_branches_in_expr(ap, fenv_access, live_refs),
        Expr::Offsetof { indices, .. } => {
            for i in indices {
                fold_branches_in_expr(i, fenv_access, live_refs);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => fold_branches_in_expr(x, fenv_access, live_refs),
                    InitItem::Designated {
                        value,
                        designator: _,
                    } => fold_branches_in_expr(value, fenv_access, live_refs),
                }
            }
        },
        Expr::AsmExpr { outputs, inputs, .. } => {
            for o in outputs {
                fold_branches_in_expr(o, fenv_access, live_refs);
            }
            for i in inputs {
                fold_branches_in_expr(i, fenv_access, live_refs);
            }
        },
        Expr::StmtExpr { block, .. } => {
            // 块体走与函数体完全同一条路径：里面的语句再进 `fold_branches_stmt`，
            // 所以嵌套的 `({...})` 与 `switch` 都能被看到。
            fold_branches_block(&mut block.items, fenv_access, live_refs);
        },
        _ => {},
    }
}

/// 「这条语句确定不会落到下一条」的判据：常量 switch 只保留命中那条臂的前提。
/// 除了 `break`，内核那一族宏里大量的臂是以 `return`/`goto` 收尾的（只看 `break`
/// 就折不掉它们：实测 `tty_ldsem.o` 13 条陷阱引用里剩下 6 条就是这种形状）。
fn stmt_stops_control(s: &Stmt) -> bool {
    match s {
        Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Return { .. }
        | Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. } => true,
        Stmt::Block { b, .. } => match b.items.last() {
            Some(BlockItem::Stmt(ss)) => stmt_stops_control(ss),
            _ => false,
        },
        // 两条分支都停才算停；缺 else 时假分支会落空，不算。
        Stmt::If {
            then_,
            else_: Some(el),
            ..
        } => stmt_stops_control(then_) && stmt_stops_control(el),
        _ => false,
    }
}

/// 只给 `DCC_FOLD_TRACE` 诊断用的表达式形状摘要（不参与任何判据）。
fn expr_fold_trace_shape(e: &Expr) -> String {
    match e {
        Expr::Ident { name, .. } => format!("Ident({})", name.name),
        Expr::Member { expr, .. } => format!("Member({})", expr_fold_trace_shape(expr)),
        Expr::Deref { e, .. } => format!("Deref({})", expr_fold_trace_shape(e)),
        Expr::Cast { e, .. } => format!("Cast({})", expr_fold_trace_shape(e)),
        Expr::Call { callee, args, .. } => format!(
            "Call({}, {})",
            expr_fold_trace_shape(callee),
            args.len()
        ),
        Expr::Binop { op, .. } => format!("Binop({op:?})"),
        Expr::SizeofTy { .. } => "SizeofTy".to_string(),
        Expr::SizeofExpr { .. } => "SizeofExpr".to_string(),
        Expr::StmtExpr { .. } => "StmtExpr".to_string(),
        Expr::Int { v, .. } => format!("Int({v})"),
        other => format!("{other:?}").chars().take(40).collect(),
    }
}

/// 递归 + 就地塌缩一条 `if` 的分支操作数（`then_`/`else_` 这些**不是块内一项**的位置）。
fn fold_branches_operand(
    s: &mut Stmt,
    fenv_access: bool,
    live_refs: &std::collections::HashSet<String>,
) {
    fold_branches_stmt(s, fenv_access, live_refs);
    collapse_const_if_operand(s, fenv_access, live_refs);
}

/// 【`else if (常量)` 就地塌缩】[`fold_branches_block`] 只能删「作为块内一项」的 `if`
/// （删/替需要拿 `Vec<BlockItem>` 的下标），而
/// ```c
/// if (lru_gen_enabled() && …) { … }            /* 运行时条件，折不动 */
/// else if (pvmw.pte) { … }
/// else if (IS_ENABLED(CONFIG_TRANSPARENT_HUGEPAGE)) {   /* THP 关 ⇒ 恒假 */
///         if (pmdp_clear_flush_young_notify(vma, address, pvmw.pmd))
///                 referenced++;
/// } else { WARN_ON_ONCE(1); }
/// ```
/// 的第三支是**外层 `if` 的 `else_` 操作数**，不是块项 ⇒ 以前永远删不掉。现场后果
/// （实测 `mm/rmap.o` 的 `folio_referenced_one`）：THP 关时这个 API 只有
/// `include/linux/pgtable.h:546` 的 `BUILD_BUG()` 占位实现，占位里的调用发进 `.text`
/// ⇒ 链接期 `undefined reference to 'pmdp_clear_flush_young'`。同一机制还罩着
/// `drivers/md/dm.o` 的 `dax_get_private`/`dm_zone_endio` 与
/// `drivers/iommu/generic_pt/**.o` 的 `__pt_no_sw_bit`（都是恒假支里的占位陷阱）。
///
/// 安全性与块级折叠同口径：条件里挂着未解析的 `__builtin_constant_p` 时不动；
/// 条件本身需要代码（[`crate::semantics::discarded_value_needs_code`]）时原样留成
/// 一条语句，不偷偷丢副作用。
fn collapse_const_if_operand(
    s: &mut Stmt,
    fenv_access: bool,
    live_refs: &std::collections::HashSet<String>,
) {
    // 先算出替代树再赋值：手里拿着 `s` 内部字段的可变借用时不能改 `*s`。
    let replacement: Option<Stmt> = match s {
        Stmt::If {
            cond, then_, else_, span,
        } => {
            if expr_has_unresolved_constp(cond) {
                None
            } else {
                match const_eval::eval(cond) {
                    Ok(v) => {
                        let taken: Option<&Stmt> =
                            if v != 0 { Some(then_.as_ref()) } else { else_.as_deref() };
                        // 被丢掉的另一臂里如果定义了「臂外还有人跳」的标号，就不能就塌缩：
                        // 标号是函数级作用域，删了定义而留了引用就是发射期 E1103
                        // （判据见 [`branch_drop_loses_live_label`]；`unsafe_get_user()`
                        // 的 `if (0) { local_label: … }` 正是坐在 `else`/`then` 操作数位上）。
                        let dropped: Option<&Stmt> =
                            if v != 0 { else_.as_deref() } else { Some(then_.as_ref()) };
                        if dropped
                            .map(|d| branch_drop_loses_live_label(d, live_refs))
                            .unwrap_or(false)
                        {
                            return;
                        }
                        let mut items: Vec<BlockItem> = Vec::new();
                        if crate::semantics::discarded_value_needs_code(cond) {
                            items.push(BlockItem::Stmt(Stmt::Expr {
                                e: cond.clone(),
                                span: span.clone(),
                            }));
                        }
                        if let Some(b) = taken {
                            items.push(BlockItem::Stmt(b.clone()));
                        }
                        Some(Stmt::Block {
                            b: Block { items, span: span.clone() },
                            span: span.clone(),
                        })
                    },
                    Err(_) => None,
                }
            }
        },
        _ => None,
    };
    if let Some(r) = replacement {
        *s = r;
        // 塌缩后可能又露出一条常量 `if`（`else if (0) … else if (0) …` 连着写），
        // 交给同一个入口继续；每步都取更小的子树，终止性由此保证。
        fold_branches_stmt(s, fenv_access, live_refs);
    }
}

fn fold_branches_stmt(
    s: &mut Stmt,
    fenv_access: bool,
    live_refs: &std::collections::HashSet<String>,
) {
    match s {
        Stmt::If {
            cond, then_, else_, ..
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            // `if (({ switch (size) {…} x; }))` 这类「条件里坐语句表达式」的形状也要下探：
            // 内核 `__cmpxchg`/`__xchg` 宏的 `({...})` 常出现在 `while (!cmpxchg(...))`、
            // `if (cmpxchg(...) == old)` 的条件位上，不下探那里的 switch 就折不掉。
            fold_branches_in_expr(cond, fenv_access, live_refs);
            // 两个分支走「递归 + 就地塌缩」（理由见 [`collapse_const_if_operand`]）。
            fold_branches_operand(then_, fenv_access, live_refs);
            if let Some(e) = else_ {
                fold_branches_operand(e, fenv_access, live_refs);
            }
        },
        Stmt::While {
            cond, body, span,
        } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_branches_in_expr(cond, fenv_access, live_refs);
            if let Ok(v) = const_eval::eval(cond) {
                if v == 0
                    && !branch_drop_loses_live_label(body, live_refs)
                    && !expr_has_unresolved_constp(cond)
                {
                    // 循环体删了，但条件的那次求值还在：`while ((x = 0));`
                    // 必须在折完后仍然写一次 x（与 `fold_if` 同一做法）。
                    let mut items = Vec::new();
                    if crate::semantics::discarded_value_needs_code(cond) {
                        items.push(BlockItem::Stmt(Stmt::Expr {
                            e: cond.clone(),
                            span: span.clone(),
                        }));
                    }
                    *s = Stmt::Block {
                        b: Block {
                            items,
                            span: span.clone(),
                        },
                        span: span.clone(),
                    };
                    return;
                }
            }
            fold_branches_stmt(body, fenv_access, live_refs);
        },
        Stmt::Block { b, .. } => fold_branches_block(&mut b.items, fenv_access, live_refs),
        Stmt::For {
            init,
            cond,
            step,
            body,
            span,
        } => {
            if let Some(c) = cond {
                *c = fold_expr(c.clone(), fenv_access);
                fold_branches_in_expr(c, fenv_access, live_refs);
                if let Ok(v) = const_eval::eval(c) {
                    if v == 0
                        && !branch_drop_loses_live_label(body, live_refs)
                        && !expr_has_unresolved_constp(c)
                    {
                        // 同 `while`：init 之后、循环之前还有一次条件求值。
                        let mut items: Vec<BlockItem> = Vec::new();
                        if let Some(i) = init.take() {
                            items.push(BlockItem::Stmt(Stmt::Expr {
                                e: stmt_to_void_expr(*i),
                                span: span.clone(),
                            }));
                        }
                        if crate::semantics::discarded_value_needs_code(c) {
                            items.push(BlockItem::Stmt(Stmt::Expr {
                                e: c.clone(),
                                span: span.clone(),
                            }));
                        }
                        *s = Stmt::Block {
                            b: Block {
                                items,
                                span: span.clone(),
                            },
                            span: span.clone(),
                        };
                        return;
                    }
                }
            }
            if let Some(i) = init {
                fold_branches_stmt(i, fenv_access, live_refs);
            }
            if let Some(st) = step {
                *st = fold_expr(st.clone(), fenv_access);
                fold_branches_in_expr(st, fenv_access, live_refs);
            }
            fold_branches_stmt(body, fenv_access, live_refs);
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            span,
        } => {
            // 这一臂以前整个落在 `_ => {}` 里：switch 里面的死分支从来没被折过。
            // 先无条件递归（prelude + 每个臂体），再试「常量选择子」这一招。
            //
            // 为什么常量 switch 必须折：内核那一族「编译期尺寸检查」宏长这样
            //   switch (sizeof(*(ptr))) { case 1: …; break; … default: __cmpxchg_wrong_size(); }
            // （`__compiletime_error`，指望编译器自己把错的尺寸在编译期拦下）。gcc 折完
            // 后**根本不会**引用那些故意不存在的陷阱函数；不折就把它们全发成未定义引用，
            // 最终链接直接失败 —— 实测本轮 `vmlinux` 链接就卡在这：
            // `__xadd_wrong_size` 269 处、`__bad_size_call_parameter` 261 处、
            // `__cmpxchg_wrong_size` 242 处、`__xchg_wrong_size` 53 处、`__get_user_bad`/`__put_user_bad` 各 ~50 处。
            for item in prelude.iter_mut() {
                if let BlockItem::Stmt(ss) = item {
                    fold_branches_stmt(ss, fenv_access, live_refs);
                }
            }
            for c in cases.iter_mut() {
                fold_branches_block(&mut c.body.items, fenv_access, live_refs);
            }
            *expr = fold_expr(expr.clone(), fenv_access);
            // 选择子里挂着未解析的 `__builtin_constant_p` 时不折（同上：此刻只能答 0，
            // 按那个 0 选臂会把「本该保留的臂」当成死臂处理）。
            if expr_has_unresolved_constp(expr) {
                return;
            }
            let Ok(sel) = const_eval::eval(expr) else {
                // 临时诊断（环内变量名不污染行为）：为什么这一跳没折
                if std::env::var_os("DCC_FOLD_TRACE").is_some() {
                    eprintln!(
                        "FOLDSWITCH bail=selector-not-const sel={}",
                        expr_fold_trace_shape(expr)
                    );
                }
                return;
            };
            // 命中点：先找值相等的 case（含 GNU `low ... high` 区间形），其次 default
            let mut hit: Option<usize> = None;
            let mut dflt: Option<usize> = None;
            for (i, c) in cases.iter().enumerate() {
                let Some(v) = c.val.as_ref() else {
                    if dflt.is_none() {
                        dflt = Some(i);
                    }
                    continue;
                };
                let Ok(lo) = const_eval::eval(v) else { continue };
                let matched = match c.range_end.as_ref() {
                    Some(re) => match const_eval::eval(re) {
                        Ok(hi) => sel >= lo && sel <= hi,
                        Err(_) => false,
                    },
                    None => sel == lo,
                };
                if matched {
                    hit = Some(i);
                    break;
                }
            }
            let take = hit.or(dflt);
            // 拼好的保留臂体（`None` = 没命中也没 default，或 take 为 None）
            let mut folded_cases: Option<Vec<BlockItem>> = None;
            // 保留范不是「只有命中那一臂」：C 的 fall-through 会从命中的臂一路跑进后面的臂。
            // 所以从命中臂开始**往下拼接**臂体，直到遇到第一个顶层 `break`（那条 break 自身
            // 也删掉，它跳的 switch 已经不存在）；在此之后的臂与命中点之前的臂一起当作死代码。
            // 只看顶层 item 是对的：嵌套 switch/循环里的 `break` 属于它们自己。
            // 两个保守退路：① 拼接链走到最后一条臂仍没碰到 `break` ⇒ 控制流会从 switch 尾部
            // 落空，后面的臂不能删，直接不折；② 被丢的臂里有标签 ⇒ 本函数里可能有 `goto`
            // 跳进去（与 `fold_branches_block` 对死分支里 `Stmt::Label` 同一条拦网）。
            if let Some(i) = take {
                // 从命中臂开始往下拼接到第一个顶层 `break`，同时记下区间尾 `kept_end`。
                //
                // 必须再看一层「臂体整体是个带花括号的块」的形状：内核这一族宏写的就是
                // `case 8: { volatile u64 *__ptr = …; asm(…); break; }` —— `break` 在块里而不是
                // 臂的顶层。只认顶层 `break` 会把这条臂当成「落空」而接着往下拼到
                // `default: __cmpxchg_wrong_size();`，把本来不会发码的陷阱调用**拼活了**
                // （实测：这么改完 printk_ringbuffer.o 的陷阱重定位从 9 涨到 15）。
                // 正确做法：遇到「块以 break 收尾」就把块里的项拆出来、去掉那条 break
                // （switch 已经不存在，break 留在树里反而会绑到外层循环上去），然后停。
                let mut kept: Vec<BlockItem> = Vec::new();
                let mut kept_end = i;
                let mut stopped = false;
                'arms: for (k, c) in cases[i..].iter().enumerate() {
                    kept_end = i + k;
                    for it in &c.body.items {
                        match it {
                            BlockItem::Stmt(Stmt::Break { .. }) => {
                                stopped = true;
                                break 'arms;
                            },
                            BlockItem::Stmt(Stmt::Block { b, .. }) => {
                                // 臂体整体是个块时，要看的不是「块里有没有 break」，而是
                                // **块的收尾是不是终止语句**：块尾是 `break` 就拆掉它再停，
                                // 块尾是 `return`/`goto`/`continue` 同样不会再落到下一臂，
                                // 只是那句终止语句本身要留下。
                                //
                                // 只认 `break` 的旧版把 `case 2: { return; }` 当成「落空」
                                // ⇒ 拼到最后一条臂仍 `!stopped` ⇒ 整体不折。带花括号的臂是
                                // 内联产物（`return;` 已经改写成 `goto end;`，外面套着 callee
                                // 的块），于是 `switch (__inlN$_target)` 这种**选择子已是常量**
                                // 的 switch 折不掉，`case ss_done: __scoped_seqlock_invalid_target();`
                                // 留在产物里（实测 `kernel/sched/build_policy.o`）。
                                let tail = b.items.last();
                                let tail_break = matches!(tail, Some(BlockItem::Stmt(Stmt::Break { .. })));
                                let tail_term = matches!(
                                    tail,
                                    Some(BlockItem::Stmt(
                                        Stmt::Break { .. }
                                            | Stmt::Return { .. }
                                            | Stmt::Goto { .. }
                                            | Stmt::Continue { .. }
                                    ))
                                );
                                if tail_term {
                                    let n = b.items.len();
                                    for inner in &b.items[..n - 1] {
                                        kept.push(inner.clone());
                                    }
                                    // `break` 跳的那条 switch 已经不存在，留着会绑到外层循环上。
                                    if !tail_break {
                                        kept.push(b.items[n - 1].clone());
                                    }
                                    stopped = true;
                                    break 'arms;
                                }
                                kept.push(it.clone());
                            },
                            other => {
                                kept.push(other.clone());
                                // `return`/`goto` 也是终止（控制流不会再落到下一臂）
                                if matches!(
                                    other,
                                    BlockItem::Stmt(
                                        Stmt::Return { .. } | Stmt::Goto { .. } | Stmt::Continue { .. }
                                    )
                                ) {
                                    stopped = true;
                                    break 'arms;
                                }
                            },
                        }
                    }
                }
                if !stopped {
                    // 一路落空到 switch 尾：后面的臂删不得，整体不折。
                    return;
                }
                // 保留区间 [i..=kept_end] 之外的臂都是死代码；它们中间只要有标签，
                // 本函数里可能有 `goto` 跳进来 ⇒ 不折（保留区间里的标签随臂体一起留下，
                // `goto` 仍能命中，平安）。
                let arm_has_label = |idx: usize| {
                    cases[idx].body.items.iter().any(|it| match it {
                        BlockItem::Stmt(ss) => stmt_has_label(ss),
                        BlockItem::Decl(Decl::Func(f)) => {
                            f.body.as_ref().map(block_has_label).unwrap_or(false)
                        },
                        _ => false,
                    })
                };
                if (0..cases.len()).any(|j| (j < i || j > kept_end) && arm_has_label(j)) {
                    if std::env::var_os("DCC_FOLD_TRACE").is_some() {
                        eprintln!("FOLDSWITCH bail=label-in-dropped-arm sel={sel}");
                    }
                    return;
                }
                // 把拼好的臂体暂存下来，下面统一接到 out 里。
                folded_cases = Some(kept);
            }
            // 选择子一定求值（C 语义），它的副作用全部以一条表达式语句保留；
            // 没有命中也没有 default 时，switch 的全部效果就只剩这一条求值。
            let mut out: Vec<BlockItem> = std::mem::take(prelude);
            out.push(BlockItem::Stmt(Stmt::Expr {
                e: expr.clone(),
                span: span.clone(),
            }));
            if let Some(kept) = folded_cases {
                out.extend(kept);
                // 补落网：上面只摘掉了「臂顶层」与「块尾」的 break，**嵌在 if/裸块里**
                // 的 break 还留在树里。`switch` 一旦删掉，这些 break 就会绑到外层
                // 循环上（C 的 break 只认最近的循环/switch，switch 没了就轮到循环）。
                // 内核实测形状（include/linux/rmap.h `__folio_try_dup_anon_rmap` 的
                // `case PGTABLE_LEVEL_PTE`）：`if (!folio_test_large(folio)) { …;
                // atomic_inc(&folio->_mapcount); break; }` —— 这个 switch 被内联进
                // `copy_pte_range` 的 `do { … } while` 后，break 变成「跳出整个页表
                // 拷贝循环」：`mm/memory.o` 里 5 处 `lock incl (%rbx)`（_mapcount）
                // 后面直接 `jmp` 到循环出口的 `_raw_spin_unlock`，跳过 rmap 记账与
                // `__copy_present_ptes`，fork 出来的子进程页表没建立。
                // 改成跳到块尾的合成标号：语义与原来的「跳出 switch」逐字相同。
                let exit = Ident {
                    name: format!(
                        "__inl{}__swend",
                        STMT_INLINE_UID.fetch_add(1, Ordering::Relaxed)
                    ),
                    span: span.clone(),
                };
                let mut nested_break = false;
                for it in out.iter_mut() {
                    if let BlockItem::Stmt(st) = it {
                        nested_break |= rewrite_breaks_to(st, &exit);
                    }
                }
                if nested_break {
                    out.push(BlockItem::Stmt(Stmt::Label {
                        name: exit,
                        stmt: Box::new(Stmt::Block {
                            b: Block {
                                items: vec![],
                                span: span.clone(),
                            },
                            span: span.clone(),
                        }),
                        span: span.clone(),
                    }));
                }
            }
            *s = Stmt::Block {
                b: Block {
                    items: out,
                    span: span.clone(),
                },
                span: span.clone(),
            };
        },
        // `do { … } while (cond)` 的臂体以前根本没人递归：内核里 `lockdep_assert()`、
        // `ctx_lock_assert()` 这一族宏都是 `do { … } while (0)` 包的块，里面的死分支
        // （`if (lockdep_is_held(l) != …)` 一类）因此从来没被折过 ⇒ 引到了
        // `!CONFIG_PROVE_LOCKING` 下**根本没定义**的 `extern int lockdep_is_held()`
        // （实测链接报 96 处，来自 `fs/file.o`、`kernel/workqueue.o`、`kernel/sched/fair.o` 等）。
        // do-while 的体一定执行一次，所以只做下探 + 折条件，不改整条语句的结构。
        Stmt::Do { body, cond, .. } => {
            *cond = fold_expr(cond.clone(), fenv_access);
            fold_branches_in_expr(cond, fenv_access, live_refs);
            fold_branches_stmt(body, fenv_access, live_refs);
        },
        // `return ({ … })`：语句表达式当返回值时，里面的 switch / 死分支同样必须被看到。
        // 内核这一族宏恰好就是这个形状：
        //   `return ({ __typeof__(…) __ret; switch (sizeof(*(&v->counter))) { case 1: …; break;
        //    … default: __cmpxchg_wrong_size(); } __ret; });`
        // 缺这一臂时整条链根本没人下探，折叠判据写得再对也轮不到它跑——实测残留的陷阱
        // 引用全从这里来：`mm/slab_common.o` 3 处、`kernel/printk/printk_ringbuffer.o` 9 处、
        // `kernel/sched/core.o` 6 处，且 `DCC_FOLD_TRACE` 对它们一条 bail 都不打（因为压根没到）。
        Stmt::Return { val: Some(e), .. } => fold_branches_in_expr(e, fenv_access, live_refs),
        // `Stmt::Label { stmt }` 把「标签」和「它带的这一条语句」绑成一个节点：不下探的话，
        // 被标签包住的整子树（强制内联拼接后的 `({...})` 里常见这种形状）永远进不了折叠。
        Stmt::Label { stmt, .. } => fold_branches_stmt(stmt, fenv_access, live_refs),
        Stmt::Expr { e, .. } => {
            // 还必须进这一臂，否则上面的 `Stmt::Switch` 形同不存在：内核那一族尺寸检查宏
            // 展开后是 `({ switch (sizeof(*(p))) { … default: __xadd_wrong_size(); } … })`
            // （`arch/x86/include/asm/rmw*.h`、`include/linux/atomic/*`），switch 坐在
            // **语句表达式**里而不是语句里 ⇒ 整批「故意不存在」的陷阱函数被发成
            // 未定义引用（实测 `tty_ldsem.o` 13 条真引用，全树 900+ 条）。
            fold_branches_in_expr(e, fenv_access, live_refs);
        },
        _ => {},
    }
}

/// 把**属于被常量 switch 折叠掉的那条 switch** 的 `break` 改写成 `goto label`。
///
/// 递归边界只有「if / 裸块 / 带标号语句」：循环与嵌套 `switch` 有自己的 break
/// 归宿，跨过去改写就是另一个语义错误。语句表达式里的 break 与
/// [`rewrite_returns_to_dest`] 同一条口径（不下探），当前形状用不到。
fn rewrite_breaks_to(s: &mut Stmt, label: &Ident) -> bool {
    match s {
        Stmt::Break { span } => {
            *s = Stmt::Goto {
                target: label.clone(),
                span: span.clone(),
            };
            true
        },
        Stmt::If { then_, else_, .. } => {
            let mut hit = rewrite_breaks_to(then_, label);
            if let Some(e) = else_ {
                hit |= rewrite_breaks_to(e, label);
            }
            hit
        },
        Stmt::Block { b, .. } => {
            let mut hit = false;
            for it in b.items.iter_mut() {
                if let BlockItem::Stmt(st) = it {
                    hit |= rewrite_breaks_to(st, label);
                }
            }
            hit
        },
        Stmt::Label { stmt, .. } => rewrite_breaks_to(stmt, label),
        _ => false,
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
    // 位置式扫（不是 `retain`）：`goto` 要不要算终止得**向后看**尾段有没有标号。
    let mut keep = vec![true; items.len()];
    let mut stopped = false;
    for i in 0..items.len() {
        if stopped {
            // G2: `Stmt::Label { stmt }` 只把紧随标签的**一条语句**纳入自己，
            // 标签之后到下一个 Label/terminator 之间的顶层 item（典型形态是
            // `out: <body>; return s;` 里的 `return s;`）仍然是「跳到 out 之后
            // 可达」的路径。旧实现只保 Label 本身、把中间的 Return 全清掉 ⇒
            // `goto out` 从 `out:` 直落进下一个标签、再从函数尾 fall-off，
            // `%eax` 只剩最后一条对 `s` 的写，实测 `out:` 少 3 变成少 3+5。
            // 正确的语义是：Label 是跳入入口，遇到它就把 stopped 复位；此后
            // 的 item 保留到本 block 末尾或下一个顶层终止语句（再触发 stopped）。
            if matches!(items[i], BlockItem::Stmt(Stmt::Label { .. })) {
                stopped = false;
            } else {
                keep[i] = false;
            }
            continue;
        }
        match &items[i] {
            BlockItem::Stmt(Stmt::Return { .. }) => stopped = true,
            // `goto`/`break`/`continue` 后面同一层级的语句同样不可达（只有标号能被跳进），
            // 复位规则与 return 同。
            //
            // 为什么必须补这条（实测 `arch/x86/mm/ioremap.o` 那 7 行）：内联会把被调方的
            // `return;` 改写成 `goto __inl<N>__l__inl<N>__end;`，于是「`if (!IS_ENABLED(CONFIG_EFI))`
            // return;` 守住的整段 EFI 代码」在**调用方**里只剩一条 `goto`，尾段照样被发射
            //（`/tmp/kwefi/t.c` 五秒夹具：dcc 留 3 条 `efi_mem_type`/`efi_mem_attributes`
            // 重定位、gcc 0 条）。只认 `Return` 的实现对这一整族形状完全无效。
            BlockItem::Stmt(Stmt::Goto { .. }) => {
                // 前向 `goto` 可能跳进**后面某个嵌套块**里的标号（`goto out; { …; out: … }`），
                // 那种形状下删尾段会把跳转目标一起删掉。尾段一旦含这类标号就整段不动；
                // 顶层标号由上面的复位规则保留（它正是内联退出标号的常见落点）。
                // 内联器自造标号不算「外部可跳入」（见 [`is_inliner_label`]）。
                if !tail_blocks_truncation(&items[i + 1..]) {
                    stopped = true;
                }
            },
            BlockItem::Stmt(Stmt::Break { .. }) | BlockItem::Stmt(Stmt::Continue { .. }) => {
                if !tail_blocks_truncation(&items[i + 1..]) {
                    stopped = true;
                }
            },
            // 【块尾就是 return】`if (1) { … return X; }` 折开之后常常留下一个
            // **以 return 收尾的裸块**，块里再套同样的形状。只认顶层 `Return` 的实现
            // 对这条完全无效：`lib/vsprintf.c` 的 `pointer()` 里 `case 'A'` 折完是
            // `Expr; Block{ Block{ Return(error_string…) } }; Return(rust_fmt_argument)`，
            // 第三条不可达却照样发引用 ⇒ 链接期 `rust_fmt_argument` 未定义。
            // `goto` 型落点不在这里认（它的目标可能在后面，靠 `Label` 复位那条兜）；
            // 尾段含**非内联自造**标号时同样整段不动，理由与上面 `goto` 那条一致。
            BlockItem::Stmt(s) if stmt_never_falls_through(s) => {
                if !tail_blocks_truncation(&items[i + 1..]) {
                    stopped = true;
                }
            },
            _ => {},
        }
    }
    if keep.iter().any(|k| !k) {
        let mut idx = 0;
        items.retain(|_| {
            let k = keep[idx];
            idx += 1;
            k
        });
    }
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
        // 【漏了 switch】臂体也是块，同一条「块尾 return」规则能删的东西一模一样。
        // 只认 If/循环/裸块时，`lib/vsprintf.c` 的 `pointer()` 里 `case 'A'` 那一串
        // 就永远扫不到（现场：`rust_fmt_argument` 链接期未定义）。
        Stmt::Switch { cases, prelude, .. } => {
            dce_block(prelude);
            for c in cases.iter_mut() {
                dce_block(&mut c.body.items);
            }
        },
        // 标签只是透传：`lbl: { …; return X; }` 的尾巴同样不落地。
        Stmt::Label { stmt, .. } => dce_block_in_stmt(stmt),
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

/// 「这条语句执行完一定落到 return」——只认 `return`、两臂都如此的 `if`、尾项如此的
/// 裸块/带标号语句，以及 `goto`/`goto *`（跳转语句自己当然不会落到下一条；它的落点
/// 会不会被删由 [`tail_blocks_truncation`] 那一侧把关）。`break`/`continue` 不认：它们
/// 本来就由 `dce_block` 的专门那条处理，这里再收一遍只多风险不多收益。
fn stmt_never_falls_through(s: &Stmt) -> bool {
    match s {
        Stmt::Return { .. } | Stmt::Goto { .. } | Stmt::GotoIndirect { .. } => true,
        Stmt::Block { b, .. } => match b.items.last() {
            Some(BlockItem::Stmt(last)) => stmt_never_falls_through(last),
            _ => false,
        },
        Stmt::Label { stmt, .. } => stmt_never_falls_through(stmt),
        Stmt::If { then_, else_: Some(e), .. } => {
            stmt_never_falls_through(then_) && stmt_never_falls_through(e)
        },
        _ => false,
    }
}

/// 编译器自己生成的标号：内联器的 `__inl<N>__end`、`__inl<N>__l__inl<N>__end`
/// 这类出口标号、被它按 `__inl<N>__l` 前缀改名的源标签与 `asm goto` 落点
/// （`__inl<N>__lt_yes` / `__inl<N>__lout` …），以及常量 switch 折叠给残留
/// `break` 发的块尾标号 `__inl<N>__swend`（见 [`rewrite_breaks_to`]）。
/// 三者共用同一命名空间，源码写不出 `__inl<数字>__` 开头的标识符。
///
/// 为什么在截断判据里要把它们豁免掉：这类名字只有**同一次展开内部**的 `goto` / `asm goto`
/// 会引用，外部源码写不出来。「尾段里有没有可能被外部跳进来的落点」这个问题的
/// 正确答案因此不是「有没有标号」，而是「有没有非内联自造标号」—— 按前者判会把
/// 每一条带 `cpu_feature_enabled()`（`asm goto`）的内联尾巴都锁住，实测
/// `arch/x86/kernel/fpu/signal.c` 的 `convert_to_fxsr` 就是这样留下来的。
/// 同一条例外也适用于死分支折叠（[`stmt_has_label`]）：`gdt_idt.c` 的 `rip_rel_ptr(vc_no_ghcb)`
/// 坐在 `if (0)` 里，内联留下的出口标号把拦网触发 ⇒ `vc_no_ghcb` 留成未定义引用。
fn is_inliner_label(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("__inl") else {
        return false;
    };
    match rest.split_once("__") {
        Some((uid, tail)) => {
            !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()) && !tail.is_empty()
        },
        None => false,
    }
}

/// 终止语句之后的这段里，是否坐着「可能被外部 `goto` 跳进来」的落点。
///
/// 与 [`item_has_any_label`] 的区别只在一点：内联器自造标号不算（理由见
/// [`is_inliner_label`]）。本层（`tail` 里顶层）的 `Stmt::Label` 自身不算 —— 它正是
/// [`dce_block`] 复位规则要保留的入口，只看它包着的那条语句里还有没有别的标号。
fn tail_blocks_truncation(tail: &[BlockItem]) -> bool {
    tail.iter().any(|it| {
        let names = match it {
            BlockItem::Stmt(Stmt::Label { stmt, .. }) => {
                crate::ast_dispatch::label_defs_of_stmt(stmt)
            },
            _ => crate::ast_dispatch::label_defs_of_item(it),
        };
        names.iter().any(|n| !is_inliner_label(n))
    })
}

/// 内联入口: 收集模块内所有函数定义, 然后逐函数内联.
///
/// `level` 控制内联激进程度：
///   - O0: 仅 `always_inline` / `flatten`
///   - O1-O2: 同 O0, 但 hot caller (profile-guided) 享受放宽阈值
///   - O3: 额外启发式小型 static 零参函数 (cost ≤ 20 AST nodes)
/// 「体只有 `return <编译期常量>;`」的 `static` 桩：把对它的调用直接换成那个常量。
///
/// 现场理由（实测 `mm/shmem.o` 里 5 条 `__compiletime_assert_NNN` 的源头）：THP 关掉时
/// `mm/shmem.c:1917` 是 `static unsigned long shmem_suitable_orders(...) { return 0; }`，
/// 而调用点是
///     suitable_orders = shmem_suitable_orders(…);
///     while (suitable_orders) { …里面是 HPAGE_PMD_* 的 BUILD_BUG() 陷阱… }
/// 循环条件就是那个常量 0 ⇒ gcc -O2 把整个循环体当死代码删掉。dcc 的 `function_inline`
/// 只展开 `__always_inline`/预算内的候选，这种普通 `static` 桩不展开，于是循环活着、
/// 陷阱引用留下 ⇒ **链接不过**（不是「少一个优化」）。
///
/// 为什么这条本身零风险：返回值与形参无关且是编译期常量；`static` 保证别的 TU 调不进来
/// （不拆定义，只换调用）。唯一要照顾的是**实参自己的求值**：不在白名单里的实参一律
/// 保留（逗号左侧），白名单只放结构上不可能有副作用的形状，与
/// [`discarded_statement_is_dead`] 同一个口径。
///
/// **指针常量也算**（只认整型时错过一整族）：`include/linux/bio-integrity.h:88` 在
/// `CONFIG_BLK_DEV_INTEGRITY` 关闭时是
///     static inline struct bio_integrity_payload *bio_integrity(struct bio *bio) { return NULL; }
/// 而调用点是 `if (bio_integrity(bio)) fs_bio_integrity_free(bio);`，那个被守住的函数在全树
/// 没有定义。实测（`/tmp/kwred/bip_stub.c` 五秒夹具）：gcc -O2 不留引用，旧实现留 1 条
/// ⇒ 全树 `fs_bio_integrity_*` 18 行与 `blk_crypto_*` 14 行未定义引用全部仍在。
/// 同时带一条反向对照：桩返非 NULL 时引用**必须留着**，所以下面不能只看函数名一推了之。
///
/// 【同 TU 内「空体 void 函数」的调用就地吸收】
///
/// 为什么这是链接必经的一步而不是「少一个优化」（`arch/x86/kernel/alternative.c:2439`）：
/// ```c
/// /* 声明与调用点都是无条件的（alternative.h:89 / alternative.c:2439）：*/
/// extern s32 __ibt_endbr_seal[], __ibt_endbr_seal_end[];
/// apply_seal_endbr(__ibt_endbr_seal, __ibt_endbr_seal_end);
/// /* 而 !CONFIG_X86_KERNEL_IBT 时被选中的定义体是（alternative.c:1191）：*/
/// void __init_or_module apply_seal_endbr(s32 *start, s32 *end) { }
/// ```
/// 那对外部数组在 `CONFIG_X86_KERNEL_IBT=n` 下**链接脚本里没有**
/// （`arch/x86/kernel/vmlinux.lds.S:277` 把 `.ibt_endbr_seal` 整个节包在
/// `#ifdef CONFIG_X86_KERNEL_IBT` 里），所以只要调用还在，最终链接就报
/// `undefined reference to '__ibt_endbr_seal'`。gcc/clang 靠「同 TU 空体函数被就地吸收
/// ⇒ 实参成死值 ⇒ 连地址都不算」把它消掉；只做 `static` 常量桩代回的
/// [`fold_const_return_calls`] 看不见这一族（它不是 static，也没有返回值）。
///
/// 不吸收的情形（每一条都可能改变可见语义，宁可留着）：
///   * `weak` 与非 `static` 的 `inline`（本代码库把后者发成 weak 符号）——
///     链接期可能被别的 TU 的强定义顶掉，那就不是空体了；
///   * 有 `alias_name` / 是 `constructor`/`destructor` / 嵌套函数；
///   * 返回类型不是 `void`（有返回值的调用可能被用作条件，替身不是它自己的值）；
///   * 任何一个实参有可观察副作用（[`crate::semantics::discarded_value_needs_code`]
///     答「需要代码」），此时整个调用原样保留。
fn fold_empty_void_calls(module: &mut Module) {
    let prog = module.source_mut();
    let mut empty: std::collections::HashSet<String> = std::collections::HashSet::new();
    for d in prog.decls.iter() {
        let Decl::Func(f) = d else { continue };
        if f.is_weak
            || f.is_inline
            || f.is_nested
            || f.is_constructor
            || f.is_destructor
            || f.alias_name.is_some()
        {
            continue;
        }
        if !matches!(f.ret_ty, Type::Void) {
            continue;
        }
        let Some(b) = &f.body else { continue };
        if b.items.is_empty() {
            empty.insert(f.name.name.clone());
        }
    }
    if empty.is_empty() {
        return;
    }
    for d in prog.decls.iter_mut() {
        let Decl::Func(f) = d else { continue };
        let Some(body) = &mut f.body else { continue };
        fold_empty_void_block(body, &empty);
    }
}

fn fold_empty_void_block(b: &mut Block, empty: &std::collections::HashSet<String>) {
    for it in &mut b.items {
        match it {
            BlockItem::Stmt(s) => fold_empty_void_stmt(s, empty),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    fold_empty_void_expr(init, empty);
                }
            },
            BlockItem::Decl(_) => {},
        }
    }
}

fn fold_empty_void_stmt(s: &mut Stmt, empty: &std::collections::HashSet<String>) {
    match s {
        Stmt::Expr { e, .. } => {
            if let Some(ph) = empty_void_call_placeholder(e, empty) {
                *e = ph;
            }
        },
        Stmt::Block { b, .. } => fold_empty_void_block(b, empty),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            fold_empty_void_expr(cond, empty);
            fold_empty_void_stmt(then_, empty);
            if let Some(el) = else_ {
                fold_empty_void_stmt(el, empty);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            // `While`/`Do` 的选择子是必有的 `Expr`（`For` 的才是 `Option`，下面单独一臂）。
            fold_empty_void_expr(cond, empty);
            fold_empty_void_stmt(body, empty);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(st) = init {
                fold_empty_void_stmt(st, empty);
            }
            if let Some(c) = cond {
                fold_empty_void_expr(c, empty);
            }
            if let Some(sp) = step {
                fold_empty_void_expr(sp, empty);
            }
            fold_empty_void_stmt(body, empty);
        },
        Stmt::Switch {
            expr, cases, prelude, ..
        } => {
            fold_empty_void_expr(expr, empty);
            fold_empty_void_items(prelude, empty);
            for c in cases {
                fold_empty_void_block(&mut c.body, empty);
            }
        },
        Stmt::Label { stmt, .. } => fold_empty_void_stmt(stmt, empty),
        _ => {},
    }
}

fn fold_empty_void_items(items: &mut Vec<BlockItem>, empty: &std::collections::HashSet<String>) {
    for it in items {
        match it {
            BlockItem::Stmt(s) => fold_empty_void_stmt(s, empty),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &mut v.init {
                    fold_empty_void_expr(init, empty);
                }
            },
            BlockItem::Decl(_) => {},
        }
    }
}

/// 表达式位置只往语句表达式里递：条件/选择子位置上的空体 void 调用不能拿 0 替
/// （那会改判据），递不进去就原样留着。
fn fold_empty_void_expr(e: &mut Expr, empty: &std::collections::HashSet<String>) {
    if let Expr::StmtExpr { block, .. } = e {
        fold_empty_void_block(block, empty);
    }
}

/// 整条表达式恰好是「对本 TU 空体 void 函数的调用」时返回它的替身（空语句的既有
/// 表示：值为 0 的 int 字面量），否则 None。
fn empty_void_call_placeholder(
    e: &Expr,
    empty: &std::collections::HashSet<String>,
) -> Option<Expr> {
    let Expr::Call { callee, args, span, .. } = e else { return None };
    let Expr::Ident { name, .. } = callee.as_ref() else { return None };
    if !empty.contains(&name.name) {
        return None;
    }
    for a in args {
        // 实参必须全是纯读：内核那一处传的止是 `extern s32 __ibt_endbr_seal[]` 这种
        // 数组名（地址常量），计算本身无副作用 ⇒ 连地址都不必留。
        if crate::semantics::discarded_value_needs_code(a) {
            return None;
        }
    }
    Some(Expr::Int {
        v: 0,
        span: span.clone(),
        ty: Type::Int,
    })
}

/// 调度位置：在 `function_inline` **之前**跑一次（另在内联后再跑一次兜底）。理由见
/// [`optimize_with_full_target`] 里那段实测：晚于内联时，`static inline` 桩的调用点已被
/// 换成读 `__inl0$rv` 嵌块临时量，这条 pass 就完全无从下手。
fn fold_const_return_calls(module: &mut Module) {
    let prog = module.source_mut();
    let mut consts: HashMap<String, Expr> = HashMap::new();
    // 【形参被提及、但返回值仍是编译期常量】的桩：返回表达式搬不过去（它引用了调用点
    // 没有的名字），可「值」本身在编译期就定死了。这一族以前被下面的 `mentions_local`
    // 整条挡掉，代价不是「少一个优化」而是**链接期未定义引用**：
    //
    //   `static inline bool is_rd_overutilized(struct rd *rd)
    //        { return !sched_energy_enabled() || READ_ONCE(rd->overutilized); }`
    //
    // `CONFIG_ENERGY_MODEL` 关时 `sched_energy_enabled()` 是 `return false` 桩 ⇒ `||`
    // 左侧恒真 ⇒ 返回值恒 1（C 语义里右操作数根本不求值，`const_eval` 的 `||` 也是短路
    // 的，所以它答得出 1）。代不回去的后果：`select_task_rq_fair` 里
    // `if (!is_rd_overutilized(this_rq()->rd))` 判不死 ⇒ `find_energy_efficient_cpu` →
    // `compute_energy` 一直有引用 ⇒ 链接期 `undefined reference to
    // 'sugov_effective_cpu_perf'`（它只有 `CONFIG_CPU_FREQ_GOV_SCHEDUTIL` 下才有定义）。
    //
    // 替体只放**整数值**：地址值（`return &a[3];`）搬过去就是「别的东西」，那一族仍由
    // 下面的 `mentions_local` 挡住（见 `index_ptr_base_double_index.c`）。
    let mut const_vals: HashMap<String, (i128, crate::ast::Type)> = HashMap::new();
    for d in prog.decls.iter() {
        let Decl::Func(f) = d else { continue };
        // 只认内部链接；返回类型限于整型/布尔/指针（结构体返回不是常量表达式，
        // `eval_full` 也判不出来，这里只是先把口径说窄）。
        if !f.is_static || f.is_extern || !is_scalar_result_for_stub(&f.ret_ty) {
            continue;
        }
        let Some(body) = &f.body else { continue };
        // 体不一定是「只有一条 return」：内核风格的桩几乎都带一行 `(void)bio;` 消
        // 未用形参警告（实测 `bio_has_integrity(struct biod *bio) { (void)bio;
        // return ((void *)0); }`）。前置语句只放行「`(void)<纯读>;`」—— 求值无可观察
        // 效果，整条跳过；带调用/赋值/volatile 的一律不算桩。
        let Some((last, front)) = body.items.split_last() else { continue };
        if !front
            .iter()
            .all(|it| matches!(it, BlockItem::Stmt(s) if stmt_is_void_discard(s)))
        {
            continue;
        }
        let BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) = last else {
            continue;
        };
        // 用「返回表达式本身」做替体，不自己造节点：`return NULL;` 在 parser 里已经
        // 带了目标指针类型，自己拼 `Expr::Int` 反而会把类型弄窄。
        let foldable = matches!(
            const_eval::eval_full(e, false),
            Ok(crate::const_eval::ConstValue::Int(_))
                | Ok(crate::const_eval::ConstValue::Addr { .. })
        );
        // **替体不得引用被调方的形参/局部**：代回是「按文本搬表达式」，调用点根本
        // 没有那些名字（`static void *p_of(int **a) { return &a[3]; }` 的 `&a[3]` 搬到
        // 调用点就变成了别的东西）。实测代价：`tests/fixtures/opt/index_ptr_base_double_index.c`
        // 的 `ckpt_of()` 被这么折掉后 -O1 运行即段错误（`eval_full` 对 `&a[3]` 会给出
        // `Addr{sym:"a",off:12}`，但它不是常量）。
        let mut local_names: std::collections::HashSet<String> = f
            .params
            .iter()
            .filter_map(|p| p.name.as_ref().map(|i| i.name.clone()))
            .collect();
        let mut decls: HashMap<String, usize> = HashMap::new();
        count_var_decls(&body.items, &mut decls);
        local_names.extend(decls.into_keys());
        let mentions_local = expr_mentions_name(e, &|n| local_names.contains(n));
        if foldable && !mentions_local {
            // `_Bool` 返回类型：调用点看到的是**转换后**的值（C11 6.3.1.2p1 的
            // `value != 0`），不是 `return` 表达式的原样。把 `return 512;` 搬过去
            // 会让调用点拿到 512，`!f()` 于是被折成假（§5.30：-O1 起 `main` 直接
            // `mov $1, %eax`，gcc 为 rc=0）。替体换成归一化后的整数常量。
            if matches!(&f.ret_ty, crate::ast::Type::Bool) {
                if let Ok(crate::const_eval::ConstValue::Int(v)) =
                    const_eval::eval_full(e, false)
                {
                    consts.insert(
                        f.name.name.clone(),
                        Expr::Int {
                            v: if v != 0 { 1 } else { 0 },
                            span: crate::diagnostics::Span::point("", 0, 0),
                            ty: crate::ast::Type::Bool,
                        },
                    );
                    continue;
                }
            }
            consts.insert(f.name.name.clone(), e.clone());
            continue;
        }
        // 值语境里「常量」的口径比 `eval_full` 严：它不仅要求整式可求值，还要求整式
        // **不带可观察效果**（`eval_full` 对赋值是宽松的，`(*p = 5) != 0` 会被算成 1）。
        // `const_eval_through_comma` 正是折叠层削逗号左侧 / 吸收 `&&`/`||` 用的同一条
        // 判据，方向必须一致，不能各定一套。
        // 允许「短路掉的那一侧」整段不参与求值（见 [`const_value_short_circuit`]）：
        // `READ_ONCE` 那种「类型带 volatile、但永远轮不到执行」的右操作数正是这一族。
        if mentions_local && is_int_scalar_ty(&f.ret_ty) {
            if let Some(v) = const_value_short_circuit(e) {
                // `_Bool` 返回类型：值语境里只有 0/1（C11 6.3.1.2p1），
                // 替体不能带 `return 512;` 的原始整数（同上面 consts 那一支）。
                let v = if matches!(&f.ret_ty, crate::ast::Type::Bool) && v != 0 {
                    1
                } else {
                    v
                };
                const_vals.insert(f.name.name.clone(), (v, f.ret_ty.clone()));
            }
        }
    }
    if consts.is_empty() && const_vals.is_empty() {
        return;
    }
    for d in prog.decls.iter_mut() {
        if let Decl::Func(f) = d {
            if let Some(body) = &mut f.body {
                subst_const_calls_block(body, &consts, &const_vals);
            }
        }
    }
}

/// 这条实参是不是「结构上确定没有可观察效果」的纯读（白名单，不是「问
/// `expr_has_side_effects`」——那个谓词默认臂 `_ => false` 把 `StmtExpr`/内置函数也算成
/// 无副作用，拿它当「可以丢」的依据就是静默删掉一次求值）。
fn arg_is_trivial_pure(e: &Expr) -> bool {
    if crate::ast::type_has_volatile(&e.ty()) {
        return false;
    }
    matches!(
        e,
        Expr::Int { .. }
            | Expr::Char { .. }
            | Expr::Float { .. }
            | Expr::String { .. }
            | Expr::SizeofTy { .. }
            | Expr::AlignofTy { .. }
            | Expr::Ident { .. }
    )
}

fn is_int_scalar_ty(ty: &crate::ast::Type) -> bool {
    matches!(
        ty,
        crate::ast::Type::Bool
            | crate::ast::Type::Char
            | crate::ast::Type::SChar
            | crate::ast::Type::UChar
            | crate::ast::Type::Short
            | crate::ast::Type::UShort
            | crate::ast::Type::Int
            | crate::ast::Type::UInt
            | crate::ast::Type::Long
            | crate::ast::Type::ULong
            | crate::ast::Type::LongLong
            | crate::ast::Type::ULongLong
    )
}

/// 常量桩允许的返回类型：整型/布尔/指针。指针那一条是实测驱动的（见
/// `fold_const_return_calls` 里的 `bio_integrity()` 例子）。
fn is_scalar_result_for_stub(ty: &crate::ast::Type) -> bool {
    is_int_scalar_ty(ty) || matches!(ty, crate::ast::Type::Ptr(_))
}

/// `(void)x;` 这种只为消未用警告的表达式语句：把操作数丢掉，求值无可观察效果。
/// 只认**纯读**操作数（复用 `arg_is_trivial_pure` 的白名单，它排除了 volatile），
/// 带调用/赋值/自增/内存写的都不算 —— 那些丢掉就会改语义。
fn stmt_is_void_discard(s: &Stmt) -> bool {
    let Stmt::Expr { e, .. } = s else { return false };
    let Expr::Cast { ty, e: inner, .. } = e else { return false };
    matches!(ty, crate::ast::Type::Void) && arg_is_trivial_pure(inner)
}

/// 常量桩替体的两张表：`exprs` 是「返回表达式可原样搬」的，`vals` 是「形参被提及、
/// 只能代值」的（见 `fold_const_return_calls` 的头注）。两张表只在这里成对流转，
/// 分开成两个独立参数会在某个递归点漏掉一个——那种错是静默的（少折一条）。
type ConstStubExprs = HashMap<String, Expr>;
type ConstStubVals = HashMap<String, (i128, crate::ast::Type)>;

fn subst_const_calls_block(
    b: &mut Block,
    consts: &ConstStubExprs,
    const_vals: &ConstStubVals,
) {
    for it in b.items.iter_mut() {
        match it {
            BlockItem::Stmt(s) => subst_const_calls_stmt(s, consts, const_vals),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &mut v.init {
                    subst_const_calls_expr(e, consts, const_vals);
                }
            },
            // 嵌套函数（GNU C）的体不在调用方的语句树里，不递进去就没人换它里的调用。
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &mut f.body {
                    subst_const_calls_block(body, consts, const_vals);
                }
            },
            BlockItem::Decl(_) => {},
        }
    }
}

fn subst_const_calls_stmt(s: &mut Stmt, consts: &ConstStubExprs, const_vals: &ConstStubVals) {
    match s {
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            subst_const_calls_expr(e, consts, const_vals)
        },
        Stmt::If { cond, then_, else_, .. } => {
            subst_const_calls_expr(cond, consts, const_vals);
            subst_const_calls_stmt(then_, consts, const_vals);
            if let Some(el) = else_ {
                subst_const_calls_stmt(el, consts, const_vals);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            subst_const_calls_expr(cond, consts, const_vals);
            subst_const_calls_stmt(body, consts, const_vals);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                subst_const_calls_stmt(i, consts, const_vals);
            }
            if let Some(c) = cond {
                subst_const_calls_expr(c, consts, const_vals);
            }
            if let Some(st) = step {
                subst_const_calls_expr(st, consts, const_vals);
            }
            subst_const_calls_stmt(body, consts, const_vals);
        },
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            subst_const_calls_expr(expr, consts, const_vals);
            for it in prelude.iter_mut() {
                if let BlockItem::Stmt(st) = it {
                    subst_const_calls_stmt(st, consts, const_vals);
                }
            }
            for c in cases.iter_mut() {
                subst_const_calls_block(&mut c.body, consts, const_vals);
            }
        },
        Stmt::Block { b, .. } => subst_const_calls_block(b, consts, const_vals),
        Stmt::Label { stmt, .. } => subst_const_calls_stmt(stmt, consts, const_vals),
        Stmt::GotoIndirect { expr, .. } => subst_const_calls_expr(expr, consts, const_vals),
        _ => {},
    }
}

/// 臂表与 `fold_branches_in_expr` 保持同一份形状：两个 walker 覆盖不一致，就会出现
/// 「同一个构造一条路径看得到、另一条看不到」这种最难查的不对称（本轮已付过几次学费）。
fn subst_const_calls_expr(e: &mut Expr, consts: &ConstStubExprs, const_vals: &ConstStubVals) {
    // 【代值】形参在返回表达式里被提及的桩（`is_rd_overutilized` 一族）：表达式搬不过去，
    // 但返回值在编译期已定死。需要发代码的实参按原顺序留成逗号左侧（求值顺序与一次真
    // 调用一致），其余的丢掉——判据是 `discarded_value_needs_code`，与折叠层削逗号左侧
    // 同一条。丢不掉的（带赋值/自增/调用/volatile 读）就留着：那时条件判不成常量，
    // 只是「没折成」，语义仍然正确。
    if let Expr::Call {
        callee,
        args,
        span,
        ty,
    } = e
    {
        if let Expr::Ident { name, .. } = callee.as_ref() {
            if let Some((v, rty)) = const_vals.get(&name.name) {
                let mut out = Expr::Int {
                    v: *v,
                    span: span.clone(),
                    ty: rty.clone(),
                };
                // 与下面代表达式那条同一条理由：替体的类型可能与调用点不同。
                if out.ty() != *ty {
                    out = Expr::Cast {
                        ty: ty.clone(),
                        e: Box::new(out),
                        span: span.clone(),
                    };
                }
                for k in args
                    .iter()
                    .filter(|a| crate::semantics::discarded_value_needs_code(a))
                    .rev()
                {
                    out = Expr::Comma {
                        l: Box::new(k.clone()),
                        r: Box::new(out),
                        span: span.clone(),
                        ty: ty.clone(),
                    };
                }
                *e = out;
                return;
            }
        }
    }
    match e {
        Expr::Call { callee, args, span, ty } => {
            subst_const_calls_expr(callee, consts, const_vals);
            for a in args.iter_mut() {
                subst_const_calls_expr(a, consts, const_vals);
            }
            if let Expr::Ident { name, .. } = callee.as_ref() {
                if let Some(value) = consts.get(&name.name) {
                    let kept: Vec<Expr> = args
                        .iter()
                        .filter(|a| !arg_is_trivial_pure(a))
                        .cloned()
                        .collect();
                    let mut out = value.clone();
                    // 替体自己的类型可能与调用点不同（如桩返 `void *`、调用点要 `long`）：
                    // 套一层到原调用结果类型的转型，保持四周表达式的类型不变。
                    if out.ty() != *ty {
                        out = Expr::Cast {
                            ty: ty.clone(),
                            e: Box::new(out),
                            span: span.clone(),
                        };
                    }
                    for k in kept.into_iter().rev() {
                        out = Expr::Comma {
                            l: Box::new(k),
                            r: Box::new(out),
                            span: span.clone(),
                            ty: ty.clone(),
                        };
                    }
                    *e = out;
                }
            }
        },
        Expr::Assign { l, r, .. } | Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            subst_const_calls_expr(l, consts, const_vals);
            subst_const_calls_expr(r, consts, const_vals);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => subst_const_calls_expr(x, consts, const_vals),
        Expr::Index { arr, idx, .. } => {
            subst_const_calls_expr(arr, consts, const_vals);
            subst_const_calls_expr(idx, consts, const_vals);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            subst_const_calls_expr(cond, consts, const_vals);
            subst_const_calls_expr(then_, consts, const_vals);
            subst_const_calls_expr(else_, consts, const_vals);
        },
        Expr::VaArg { ap, .. } => subst_const_calls_expr(ap, consts, const_vals),
        Expr::StmtExpr { block, .. } => subst_const_calls_block(block, consts, const_vals),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits.iter_mut() {
                match it {
                    crate::ast::InitItem::Expr(x) => subst_const_calls_expr(x, consts, const_vals),
                    crate::ast::InitItem::Designated { value, .. } => {
                        subst_const_calls_expr(value, consts, const_vals)
                    },
                }
            }
        },
        _ => {},
    }
}

/// 把「可见值恒为一个整常量」的局部代回**条件与 switch 选择子**位置。
///
/// 现场理由（§3.37 的第二半，实测 `full27` 的 253 行里 A1 占 124）：`mm/shmem.c` 里
///     unsigned long suitable_orders = 0;
///     …
///     suitable_orders = shmem_suitable_orders(…);   /* 常量桩，§3.37 已折成 0 */
///     while (suitable_orders) { …HPAGE_PMD_* 的 BUILD_BUG() 陷阱… }
/// 桩折成常量后还差这一步：常量的**落点是一个局部**，循环条件读的是变量而不是调用，
/// `fold_branches_stmt` 的 `const_eval::eval(cond)` 看不到 0 ⇒ 死循环删不掉。
///
/// 入选项（全满足才代回，且只代回条件/选择子这两类位置）：
///   1. 整型标量；非 `static`/`extern`/TLS/`cleanup`/`register`/带 asm 标号；非 volatile；
///   2. 声明在**函数体顶层**，且本函数里这个名字只声明一次（不遮蔽、不是形参）；
///   3. 初值就是一个整字面量 C；
///   4. 从不被取址、没出现在任何 asm 操作数里、没有 `++`/`--` 目标；
///   5. 每一次 `x = R` 的 R 都折成**同一个** C（shmem 的形状正是「声明时 0 + 赋常量桩返回值 0」）。
/// 这四条一起让「任何时刻读 x 都得到 C」成立，代回是等值改写，与读点在第几层块无关。
/// §3.23 那版条件位置代回被打回 10 条用例，缺的正是 4 与 5（并且它顺手动了 asm 代回时机）。
fn propagate_const_locals_in_conditions(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        let Decl::Func(f) = decl else { continue };
        // 形参名先离开 `f.body` 的可变借用拿出来（两者同时借同一个对象会撞 borrowck），
        // 并且只拿名字：后面的候选判据只问「这个名字是不是形参」。
        let param_names: std::collections::HashSet<String> = f
            .params
            .iter()
            .filter_map(|p| p.name.as_ref().map(|id| id.name.clone()))
            .collect();
        let Some(body) = &mut f.body else { continue };
        // 本 pass 不改控制流，只在条件/选择子里做等值替换；要定位哪个函数被改过，
        // 开 `DCC_OPT_SKIP=propagate_const_locals_in_conditions` 对比产物就够了。
        propagate_one_func(body, &param_names);
    }
}

fn propagate_one_func(
    body: &mut Block,
    param_names: &std::collections::HashSet<String>,
) -> bool {
    // 只声明一次的名单（包含嵌套块里的声明，用来拒绝遮蔽）
    let mut decl_counts: HashMap<String, usize> = HashMap::new();
    count_var_decls(&body.items, &mut decl_counts);
    // 候选：初值是整字面量的局部。顶层与嵌套块都收 —— 内联展开后的形参临时量就坐在
    // 嵌套块里（`UInt __inl2439$_idx = 514;` 后面紧跟读它的 `if`），只认顶层的话
    // BUILD_BUG_ON 的条件永远折不动，`__compiletime_assert_N` 的调用就留在产物里。
    let mut cands: HashMap<String, Expr> = HashMap::new();
    collect_cprop_candidates(&body.items, &decl_counts, param_names, true, true, &mut cands);
    let trace = std::env::var_os("DCC_CPROP_TRACE").is_some();
    if trace {
        let mut names: Vec<&String> = cands.keys().collect();
        names.sort();
        eprintln!("[cprop] 收到候选 {names:?}");
    }
    if cands.is_empty() {
        return false;
    }
    let consts: HashMap<String, i128> = cands
        .iter()
        .filter_map(|(n, e)| match e {
            Expr::Int { v, .. } => Some((n.clone(), *v)),
            _ => None,
        })
        .collect();
    // 筛掉候选的事实：取址、asm 操作数里的名字、++/-- 目标，以及「赋了不同的常量」
    let mut bad: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_cprop_disqualifiers(&body.items, &consts, &mut bad, false);
    cands.retain(|n, _| !bad.contains(n));
    if trace {
        let mut badv: Vec<&String> = bad.iter().collect();
        badv.sort();
        eprintln!("[cprop] 被筛掉 {badv:?}");
    }
    if cands.is_empty() {
        return false;
    }
    let clean: std::collections::HashSet<String> = std::collections::HashSet::new();
    let visible: HashMap<String, Expr> = HashMap::new();
    subst_cprop_items(&mut body.items, &cands, &visible, &clean)
}

/// `DCC_FWD_TRACE=1` 的闸门：只读一次 env（本函数在每条存储上都会被叫，直接
/// `env::var_os` 就是上万次 getenv，会扰动与它无关的计时现场）。
fn fwd_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DCC_FWD_TRACE").is_some())
}

/// 块内**向前**的常量代回：`x = 0;` 之后的读点（条件与 switch 选择子）代成 0。
///
/// 现场理由（§3.51，实测）：SSA 管线对**非结构化**函数（有 goto/label）只走
/// analysis-only 通道 —— 不重建、不跑 SCCP —— 于是「常量先存进局部、后面拿它当条件读」
/// 这一整族在核内核码里一条都折不掉，而内核到处都是 `goto out;`。`mm/shmem.c` 的
/// `shmem_alloc_and_add_folio` 就是标本：
/// ```text
/// if (!IS_ENABLED(CONFIG_TRANSPARENT_HUGEPAGE)) orders = 0;   /* 预处理成 if (1) */
/// if (orders > 0) { … if (pages == HPAGE_PMD_NR) BUILD_BUG() … }   /* 死块，5 条陷阱 */
/// ```
/// 第一个 `if` 被 [`fold_constant_branches`] 拍平后剩一行 `orders = 0;`，而它后面那条
/// `if (orders > 0)` 在 AST 层没人会看（[`propagate_const_locals_in_conditions`] 只认
/// **声明初值**，形参和被后赋值的局部都不在候选里）⇒ 死块连同 `BUILD_BUG()` 的
/// 无条件调用留在产物里 ⇒ 链接期 `undefined reference to __compiletime_assert_528`。
///
/// 与 [`propagate_const_locals_in_conditions`] 的分工：那一条是「任何时刻读 x 都是 C」的
/// 全局不变式（初值来自声明）；这一条是**位置敏感**的同一个常量，判据沿两条轴：
///   * 名字轴（函数级，一次性）：整型标量局部/形参 + 整函数只声明一次 + 地址从不逃逸
///     （取址、asm 操作数、`++`/`--` 目标）—— 与 cprop 共用 `collect_cprop_disqualifiers`；
///   * 位置轴（逐条语句）：`x = C` 之后代回，直到 x 被写、或遇到**有 goto 指向**的标号。
/// 标号那条是关键：没人跳进来的标号只是顺序执行里的一行（内联自造出口标号、死块删完
/// 剩下的孤儿标号都属此列），挡住它会把覆盖面砍掉大半；反过来，有 goto 指向的标号必须
/// 清空表 —— 那条路径刚好绕过了 `x = C`。
fn propagate_const_stores_forward(module: &mut Module) -> bool {
    let prog = module.source_mut();
    let mut changed = false;
    for decl in &mut prog.decls {
        let Decl::Func(f) = decl else { continue };
        // 形参名单与类型先取出来（`params` 与 `body` 同时可变借同一个 `f` 会撞 borrowck）
        let mut params: HashMap<String, crate::ast::Type> = HashMap::new();
        for p in &f.params {
            if let Some(n) = &p.name {
                params.insert(n.name.clone(), p.ty.clone());
            }
        }
        let Some(body) = &mut f.body else { continue };
        // 名字轴：收全函数（含嵌套块）里「整型标量 + 只声明一次」的局部。
        //
        // 为什么必须下探：内联产生的形参临时量（`bool __inl10338$rv = false;`）就声明在
        // for/if 的块里，只看函数体顶层会把它们全漏掉 —— `mm/vmstat.o` 的 2 条
        // `__compiletime_assert_483/485` 就是 `if (__inl10338$rv)` 判不出来：
        // `vmstat_item_print_in_thp()` 的 `return false;` 桩已经把值写进那个临时量，
        // 位置轴只沿顶层走则一次也代不回去。
        //
        // goto 跳过初始化这一层不靠名字轴防：位置轴每进入一层新表，只在**看到存储的那一刻**
        // 才把名字放进去（声明带初值也算存储），而 goto 目标标号会当场清表。
        let mut decl_counts: HashMap<String, usize> = HashMap::new();
        count_var_decls(&body.items, &mut decl_counts);
        let mut qual: HashMap<String, crate::ast::Type> = params.clone();
        collect_forward_candidates(&body.items, &decl_counts, &mut qual);
        qual.retain(|n, ty| !params.contains_key(n) || is_int_scalar_for_cprop(ty));
        // 地址逃逸/asm/自增自减这三类全局危害（与写值无关）⇒ 这个函数里整支作废
        let probe: HashMap<String, i128> = qual.keys().map(|n| (n.clone(), 0)).collect();
        let mut hazard: std::collections::HashSet<String> = std::collections::HashSet::new();
        collect_cprop_disqualifiers(&body.items, &probe, &mut hazard, true);
        qual.retain(|n, _| !hazard.contains(n));
        // 排障用：本 pass 的失败几乎全是「候选名根本没进表」或「某一条存储没被认出」，
        // 而这两件事从产物上看不出来（现场：`folio_is_zone_device()` 两层内联链的
        // `__inl0$rv = 0` 折不掉，`mm/swap.o` 留下 `free_zone_device_folio` 引用）。
        // 只在显式开了开关时才打印，且不参与任何判据。
        if fwd_trace_on() {
            let mut ks: Vec<&String> = qual.keys().collect();
            ks.sort();
            let mut hz: Vec<&String> = hazard.iter().collect();
            hz.sort();
            eprintln!("FWD| fn={} qual={:?} hazard={:?}", f.name.name, ks, hz);
        }
        if qual.is_empty() {
            continue;
        }
        // 位置轴：有 goto 指向的标号（含 asm goto 跳出目标），外加**每个标号的进入边数**
        // （`goto_edges` 不聚重，与 `goto_targets` 出自同一个遍历）。
        let facts = crate::ast_dispatch::collect(body);
        let mut edges: HashMap<String, usize> = HashMap::new();
        for n in &facts.goto_edges {
            *edges.entry(n.clone()).or_insert(0) += 1;
        }
        let axis = FwdAxis {
            targets: facts.goto_targets.into_iter().collect(),
            edges,
        };
        changed |= forward_items(&mut body.items, &qual, &axis);
    }
    changed
}

/// 「位置轴」事实：哪些标号有 goto 跳入，以及每个标号有几条进入边。
///
/// 为什么合成一个类型而不是两个参数：整套 walker（`forward_items`/`forward_items_with`/
/// `forward_nested*`）只传这一件事，不会一个判据传了、另一个忘了传。
struct FwdAxis {
    /// 有 `goto` 指向的标号名（聚重后的集合）。
    targets: std::collections::HashSet<String>,
    /// 标号名 → 本函数内 `goto L` 的**条数**（不聚重）。
    edges: HashMap<String, usize>,
}

impl FwdAxis {
    fn jumped(&self, name: &str) -> bool {
        self.targets.contains(name)
    }

    /// 「紧邻标号延续常量表」这条规则能不能用：只有**恰好一条**进入边时才能用。
    /// 两条以上时，另一条路带着不同的事实跳进来，沿紧邻那条递的值对它不成立。
    /// 现场：bison 骨架的 `yyreduce:` 同时被 `yybackup` 分支（`goto yyreduce;`）与
    /// `yydefault` 分支（紧邻的那条）跳入，两条路上 `yylen`/`yyn` 不同 —— 旧规则把
    /// `yylen = 0` 代进 reduce 里，`scripts/kconfig/parser.tab.c` 在 -O1 就被编坏，
    /// 症状是运行期 `input in flex scanner failed`（靠逐对象归属 + 逐 pass 二分定位）。
    fn sole_in_edge(&self, name: &str) -> bool {
        self.edges.get(name).copied().unwrap_or(0) == 1
    }
}

/// 同一层项列表的向前代回。只沿本层走：嵌套块里的读点由 `subst_cprop_stmt` 按可见表
/// 代回，而嵌套块里的**写**（可能不执行）不往外交 —— 方向是保守的。
fn forward_items(
    items: &mut [BlockItem],
    qual: &HashMap<String, crate::ast::Type>,
    targets: &FwdAxis,
) -> bool {
    let mut known: HashMap<String, Expr> = HashMap::new();
    let mut vals: HashMap<String, i128> = HashMap::new();
    let mut gi: Option<String> = None;
    let mut lctx = LabelCtx::new();
    forward_items_with(items, qual, targets, &mut lctx, &mut known, &mut vals, &mut gi)
}

/// 标号入口判据的三项事实（见 [`forward_items_with`] 里「goto 目标」那一条）。
///
/// 序号一律用**全局单调序**（[`LabelCtx::enter`] 给每一层项列表铺一段），
/// 而不是各层自己的下标：裸块递归时 `known`/`vals` 是共享的，
/// 如果 `store_at` 记的是子列表下标，「外层第 5 项存的」「子块第 3 项存的」就没法比。
struct LabelCtx {
    /// 标号名 → 指向它的**最小**项序号（没人指向的名字不出现）。
    min_src: HashMap<String, usize>,
    /// 标号名 → 指向它的**最大**项序号。与 `min_src` 拼起来就能认出**回边**：
    /// `max_src[L] > L 自己的序号` ⇒ 有后面的 `goto` 跳回这个标号，标号之后的语句会被
    /// 重跑（bison 骨架的 `yynewstate:`/`yysetstate:` 就是这一形，现场见
    /// [`forward_items_with`] 里「回边」那段）。
    max_src: HashMap<String, usize>,
    /// 项序号 → 本子树里被**写过**的候选名（与值无关，只问「有没有人写过它」）。
    /// 回边判据要问「跳回来之前这个区间内有没有写过某名字」，而当时 `items` 已被
    /// 循环可变借用，所以在本层铺开时先把每格的写集合存下来。
    writes_at: HashMap<usize, std::collections::HashSet<String>>,
    /// 名字 → 最近一次「纯常量存储」发生在哪个项序号。
    store_at: HashMap<String, usize>,
    /// 名字 → **本表覆盖到的这些层里**亲眼见到的 `goto` 条数。
    ///
    /// `FwdAxis::edges` 是**整函数**的条数（用于 [`FwdAxis::sole_in_edge`]），而本表只沿
    /// 一条层链攒事实：`switch` 的每个 case 体都是从 [`forward_items`] 另起一张表，别的臂里
    /// 的 `goto L` 在本层既数不到、也定不了位。两种条数不等就说明有本层看不见的进入边。
    local_edges: HashMap<String, usize>,
    /// 函数里出现过 `goto *expr` ⇒ 落入点点不出来，退回「标号一律清表」。
    indirect: bool,
    /// 下一个可用序号。
    next_seq: usize,
}

impl LabelCtx {
    fn new() -> Self {
        LabelCtx {
            min_src: HashMap::new(),
            max_src: HashMap::new(),
            writes_at: HashMap::new(),
            store_at: HashMap::new(),
            local_edges: HashMap::new(),
            indirect: false,
            next_seq: 0,
        }
    }

    /// 给本层项列表铺一段序号，并把层内的 goto 事实与写集合并进表；返回本层的起始序号。
    ///
    /// 每层都扫：内联造出来的 `goto end;` 与 `end:` 常常不在同一层（见
    /// [`forward_items_with`] 的「紧邻标签」那段），而内联函数体被包成了裸块，
    /// 只扫最外层的话子块里的跳转一个也看不到。
    fn enter(&mut self, items: &[BlockItem], qual: &HashMap<String, crate::ast::Type>) -> usize {
        // 名字全集拿 `qual`（本函数唯有的候选名）当种子：写集合只用来回答
        // 「候选名在区间内被动过没有」，候选集之外的人不值得记。
        let probe: HashMap<String, i128> = qual.keys().map(|n| (n.clone(), 0)).collect();
        let base = self.next_seq;
        for (i, it) in items.iter().enumerate() {
            let (names, ind) = crate::ast_dispatch::goto_facts_of_item(it);
            self.indirect |= ind;
            let seq = base + i;
            // 本格的写集合（递归整棵子树，含标号包着的语句与语句表达式里的 `({...})`）。
            let mut ws: std::collections::HashSet<String> = std::collections::HashSet::new();
            match it {
                BlockItem::Stmt(s) => collect_cprop_disq_stmt(s, &probe, &mut ws, false),
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(e) = &v.init {
                        collect_cprop_disq_expr(e, &probe, &mut ws, false);
                    }
                },
                _ => {},
            }
            if !ws.is_empty() {
                self.writes_at.insert(seq, ws);
            }
            for n in names {
                *self.local_edges.entry(n.clone()).or_insert(0) += 1;
                self.min_src
                    .entry(n.clone())
                    .and_modify(|v: &mut usize| {
                        if seq < *v {
                            *v = seq;
                        }
                    })
                    .or_insert(seq);
                self.max_src
                    .entry(n)
                    .and_modify(|v: &mut usize| {
                        if seq > *v {
                            *v = seq;
                        }
                    })
                    .or_insert(seq);
            }
        }
        self.next_seq = base + items.len() + 1;
        base
    }
}


/// [`forward_items`] 的本体：`known`/`vals` 由调用方持有，好让「紧邻标签」把同一张表
/// 继续用下去（见下面 `goto L; L:` 那一条）。
///
/// `goto_into` 也交给调用方持有，理由与表相同，但覆盖面更广一档：内联造出来的
/// `goto end;` 和 `end:` **不一定在同一层列表里**。实测 `mm/swap.c` 的
/// `folios_put_refs()`（两层 `static inline` 桩 `folio_is_zone_device` → 
/// `memdesc_is_zone_device`）展开成
/// ```text
/// { Decl __inl0$rv;                       ← 外层裸块 L2
///   { { __inl0$rv = 0; goto end; }        ← 内层裸块 L3（表的写入发生在这里）
///     end: { if (__inl0$rv) … } } }       ← L2 的下一项
/// ```
/// 控制流从 L3 末尾**直落**进 `end:`（中间没有任何分支），所以常量必须能跟着走出裸块。
/// 只允许裸块（`Stmt::Block`）这么递：`if`/`while`/`for`/`switch` 的体末尾写 `goto` 时，
/// 该体也可能整条不走，递出来就会把「可能没执行」的当代已执行。fn 名一律按
/// `decl_counts==1` 筛过，裸块里新声明的局部不可能与块外同名，共享一张表不会串台。
fn forward_items_with(
    items: &mut [BlockItem],
    qual: &HashMap<String, crate::ast::Type>,
    targets: &FwdAxis,
    lctx: &mut LabelCtx,
    known: &mut HashMap<String, Expr>,
    vals: &mut HashMap<String, i128>,
    goto_into: &mut Option<String>,
) -> bool {
    let clean: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed = false;
    let base = lctx.enter(items, qual);
    for (idx, it) in items.iter_mut().enumerate() {
        let i = base + idx;
        // 【goto 紧邻标签】`goto L; L: {…}` —— 跳到的就是下一条语句，等于空操作，
        // 所以 L **不是**一个新入口，表要延续进去。
        //
        // 这个形状是内联造的：`try_static_inline_general` 把 `return e;` 改写成
        // `rv = e; goto end;`，而 `end:` 标记的是**函数体剩余部分**，于是
        // `if (1) return false; return item == …;` 展开成
        //     rv = 0; goto end; end: { if (rv) …; … }
        // 不延续表，`rv = 0` 就进不了 `end:` 块 ⇒ `if (rv)` 折不成常量 ⇒ 死块里的
        // `BUILD_BUG()` 整片留下（实测 `mm/vmstat.o` 483/485、`:34000` 夹具）。
        if let BlockItem::Stmt(Stmt::Label { name, stmt, .. }) = it {
            // 递表只对「只有这一条进入边」的标号成立（见 [`FwdAxis::sole_in_edge`]）；
            // 多条进入边时不延续，但也不能当成「新入口」而清掉表 —— 那条路径上这些事实
            // 依旧成立，只是不能拿来替换标号之后的读点。
            if goto_into.as_deref() == Some(name.name.as_str())
                && targets.sole_in_edge(&name.name)
            {
                if fwd_trace_on() {
                    eprintln!(
                        "FWD| adjacent label={} known={} edges={}",
                        name.name,
                        known.len(),
                        targets.edges.get(&name.name).copied().unwrap_or(0)
                    );
                }
                if let Stmt::Block { b, .. } = stmt.as_mut() {
                    changed |=
                        forward_items_with(&mut b.items, qual, targets, lctx, known, vals, goto_into);
                }
                *goto_into = None;
                continue;
            }
        }
        // 【goto 目标】有 goto 指向的标号 = 一个可以绕过前面存储的入口点。
        //
        // 但「一有入口就整表清掉」砍得太多：`arch/x86/kernel/fpu/signal.c` 的
        // `copy_fpstate_to_sigframe` 里 `retry:` 只有一个下游的 `goto retry`，从那里
        // 跳回来时 `ia32_fxstate = 0` 早已执行过；整表一清，
        // `if ((ia32_fxstate || !use_fxsr()) && !save_fsave_header(tsk, buf))` 就判不出死，
        // `W save_fsave_header`（内含 `convert_from_fxsr`）整支留在产物里，链接期
        // undefined reference。
        // 判据：只有当某条 goto 的源项序号**早于**该名字的存储项时，那条路径才真的
        // 跳过了存储；源在存储之后（向后回跳）或源就排在存储之后都保留表。
        // `goto *expr` 让落入点不可枚举，这时退回旧行为（[`LabelCtx::indirect`]）。
        if let BlockItem::Stmt(Stmt::Label { name, .. }) = it {
            if targets.jumped(&name.name) {
                // 【本层看不见的进入边】整函数边数多于本层可见数 ⇒ 另有路径跳进来，而那条
                // 路径上的位置本层比不出来（`min_src` 只是本层的最小值），它完全可能绕过了
                // `x = C`。实测 `switch (c) { case 1: isz = 2; goto U;
                // case 2: isz = 4; goto U; U: switch (isz) { … } }`：case 2 那一层只看得见
                // 自己那条排在 `isz = 4` **之后**的 `goto U`，于是判「没绕过存储」把
                // `isz` 代成 4 —— case 1 走进去就落进 case 4 的臂（-O1/-O2 错值，-O0 正常）。
                // 定位不到就整表清掉（与 `indirect` 同一口径：宁可少折，不可错值）。
                let seen = lctx.local_edges.get(&name.name).copied().unwrap_or(0);
                let total = targets.edges.get(&name.name).copied().unwrap_or(0);
                if lctx.indirect || total > seen {
                    known.clear();
                    vals.clear();
                } else {
                    let src = lctx.min_src.get(&name.name).copied();
                    let skip: Vec<String> = known
                        .keys()
                        .filter(|n| match (src, lctx.store_at.get(*n)) {
                            (Some(s), Some(&at)) => s < at,
                            // 源/存储项定位不到就按旧口径清掉（宁可少折，不可错值）
                            _ => true,
                        })
                        .cloned()
                        .collect();
                    for n in skip {
                        known.remove(&n);
                        vals.remove(&n);
                    }
                    // 【回边】有 `goto L` 从**更后面**跳回本标号 ⇒ 标号之后的语句会被重跑。
                    // 代回器沿的是源码顺序轴，它看不到「跳回来再走一遍」这条路：
                    // bison 骨架里 `yy_state_fast_t yystate = 0;`（声明初值）→
                    // `yynewstate:`（被后面的 `goto yynewstate;` 回跳）→ 落到
                    // `if (yystate == YYFINAL) YYACCEPT;`。代成 `0 == 6` 就是把 accept
                    // 判掉，parser 跑过头撞上 EOF，症状是 flex 报
                    // `input in flex scanner failed`（实测 `scripts/kconfig/parser.tab.c`，
                    // -O1 起坏、-O0 正常；靠逐对象归属 + 逐 pass 二分 + 指令流对比定位）。
                    // 判据：跨度 (本标号, 最远回跳源] 内被写过的名字必须摘掉；没被写过的
                    // 名字照旧保留（`copy_fpstate_to_sigframe` 的 `ia32_fxstate` 就是那一形）。
                    if let Some(&mx) = lctx.max_src.get(&name.name) {
                        if mx > i {
                            let mut back: Vec<String> = Vec::new();
                            for j in (i + 1)..=mx {
                                if let Some(ws) = lctx.writes_at.get(&j) {
                                    for w in ws {
                                        if known.contains_key(w) {
                                            back.push(w.clone());
                                        }
                                    }
                                }
                            }
                            if !back.is_empty() && fwd_trace_on() {
                                eprintln!(
                                    "FWD| back-edge {} mx={} kills={:?}",
                                    name.name,
                                    mx,
                                    {
                                        let mut v = back.clone();
                                        v.sort();
                                        v
                                    }
                                );
                            }
                            for n in back {
                                known.remove(&n);
                                vals.remove(&n);
                            }
                        }
                    }
                }
            }
        }
        if !known.is_empty() {
            // 【顺序判据】本条语句**内部**写过谁，就必须在代回它之前从表里摘掉。
            //
            // 为什么：`subst_cprop_stmt` 是对整棵子树一次性代回，它看不到「写在其后」。
            // 两处会因此错值：
            //   * 循环体会二次进入 —— `iFirst = 0; while (iFirst < iLast-1) { … iFirst = iMid; }`
            //     把条件里的 `iFirst` 代成 0 就是死循环（实测挂死在集成用例
            //     `licm_does_not_hoist_over_loop_variant_subscript`）；
            //   * 分支里只有某条路写 —— `if (c) x = 5; else use(x);` 的 else 臂读的不是 0。
            // 摘掉只放弃**本条语句内部**的代回；它之后的读点由下面的 `moved` 重新判定
            // （那条仍是「写过就作废」，方向一致，不另升第二套规则）。
            let mut inner: std::collections::HashSet<String> = std::collections::HashSet::new();
            match it {
                BlockItem::Stmt(s) => collect_cprop_disq_stmt(s, &vals, &mut inner, false),
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(e) = &v.init {
                        collect_cprop_disq_expr(e, &vals, &mut inner, false);
                    }
                },
                _ => {},
            }
            for n in &inner {
                known.remove(n);
                vals.remove(n);
            }
        }
        if !known.is_empty() {
            // 表同时当 `cands` 与 `visible`：代回器只从 `visible` 取代换值
            //（`subst_cprop_expr_scoped` 的 Ident 臂只查 `visible`），而 candidates
            // 那一路只在碰到声明项时才往 `visible` 里加 —— 本 pass 的表是沿位置攒出来的，
            // 没有声明项可碰，两个参数必须喂同一张表（实测：只喂 cands 时一个字都不代）。
            if fwd_trace_on() {
                let mut kk: Vec<&String> = known.keys().collect();
                kk.sort();
                let kind = match it {
                    BlockItem::Stmt(Stmt::Label { .. }) => "label",
                    BlockItem::Stmt(Stmt::If { .. }) => "if",
                    BlockItem::Stmt(Stmt::Goto { .. }) => "goto",
                    BlockItem::Stmt(Stmt::Expr { .. }) => "expr",
                    BlockItem::Stmt(_) => "stmt",
                    BlockItem::Decl(_) => "decl",
                };
                eprintln!("FWD| item#{} {} known={:?}", i, kind, kk);
            }
            match it {
                BlockItem::Stmt(s) => {
                    let did = subst_cprop_stmt(s, &known, &known, &clean);
                    if fwd_trace_on() && did {
                        eprintln!("FWD| subst ok known={:?}", known.keys().collect::<Vec<_>>());
                    }
                    changed |= did;
                },
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(e) = &mut v.init {
                        changed |= subst_cprop_in_expr(e, &known, &known, &clean);
                    }
                },
                _ => {},
            }
        }
        // 本条动过谁（写值 + 逃逸 hazard 用同一张臂表，`hazards_only=false` 把「写」算进来）
        let mut moved: std::collections::HashSet<String> = std::collections::HashSet::new();
        if !vals.is_empty() {
            match it {
                BlockItem::Stmt(s) => collect_cprop_disq_stmt(s, &vals, &mut moved, false),
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(e) = &v.init {
                        collect_cprop_disq_expr(e, &vals, &mut moved, false);
                    }
                },
                _ => {},
            }
            for n in &moved {
                known.remove(n);
                vals.remove(n);
            }
        }
        // 本条就是一条纯存储 `x = <常量>` ⇒ 记进表（供后面的读点代回）
        if let Some((name, c, ty, span)) = store_const_stmt(it) {
            if fwd_trace_on() {
                eprintln!(
                    "FWD| store name={} c={} in_qual={} moved={}",
                    name,
                    c,
                    qual.contains_key(&name),
                    moved.contains(&name)
                );
            }
            if qual.contains_key(&name) && !moved.contains(&name) {
                known.insert(name.clone(), Expr::Int { v: c, span, ty });
                vals.insert(name.clone(), c);
                lctx.store_at.insert(name, i);
            }
        }
        // 「声明即存储」：`bool __inlN$rv = false;` 与 `__inlN$rv = false;` 同义。
        // 内联把 `return false;` 桩写成临时量的初值，只认赋值语句会整族漏掉
        // （`mm/vmstat.o` 的 `vmstat_item_print_in_thp` 就走这条）。
        if let BlockItem::Decl(Decl::Var(v)) = it {
            if qual.contains_key(&v.name.name) && !moved.contains(&v.name.name) {
                if let Some(e) = &v.init {
                    // 与候选表同一个值口径（[`store_value_as`]）：存进对象的是
                    // `(T)init` 转换后的值。旧写法直接拿 `eval_full`，于是
                    // `long r1 = (*dp = 300);` 把 `r1` 记成 300，`return r1;` 被代成
                    // 300 而真值是 44（`ssa_assign_value` 的 narrow 实测）。
                    if let Some(c) = store_value_as(e, &v.ty, &v.span) {
                        known.insert(
                            v.name.name.clone(),
                            Expr::Int {
                                v: c,
                                span: v.span.clone(),
                                ty: v.ty.clone(),
                            },
                        );
                        vals.insert(v.name.name.clone(), c);
                        lctx.store_at.insert(v.name.name.clone(), i);
                    }
                }
            }
        }
        // 子块另起一张表（块级作用域）：块里的写不外传，块里的读沿本块自己的位置轴走。
        // 例外是**裸块**：它没有分支，控制流直落，所以共享本表并把块尾的 goto 递出来。
        let (nested_changed, nested_goto) = forward_nested(it, qual, targets, lctx, known, vals);
        changed |= nested_changed;
        // 只认紧邻的那一种：本项是 goto（或裸块走到块尾正好是 goto），下一项刚好就是这个标号。
        *goto_into = match it {
            BlockItem::Stmt(Stmt::Goto { target, .. }) => Some(target.name.clone()),
            _ => nested_goto,
        };
    }
    changed
}

/// 把「整型标量、全函数只声明一次、不带存储类/volatile/cleanup」的局部收进候选表。
///
/// 与 [`collect_cprop_candidates`] 的分工：那个收的是「初值即常量」的全域候选，这里收的是
/// 「位置轴允许代回的名字白名单」——值仍然要等位置轴看到存储才认，所以这里不要求初值。
fn collect_forward_candidates(
    items: &[BlockItem],
    decl_counts: &HashMap<String, usize>,
    out: &mut HashMap<String, crate::ast::Type>,
) {
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => {
                // 初值必须**无条件**下探：内联把临时量声明全塞在「某个变量的初值就是
                // `({ … })`」里，而那个变量常常是指针/结构体（`is_int_scalar_for_cprop`
                // 不收）。以前两条筛选用 `continue` 提前退出，连带把初值里的整片
                // `__inlN$…` 一起看不见了：实测 `fs/xattr.o` 的 `simple_xattr_get`
                // 候选表只剩 `["ret", "size"]`，`__rhashtable_lookup()` 展开出的
                // `__inl10094$_freq` 全不在内 ⇒ `BUILD_BUG_ON(!__builtin_constant_p(freq))`
                // 永远判不成常量 ⇒ `fs/xattr.o`/`fs/pidfs.o` 各留 2 条
                // `__compiletime_assert_473/487`。「这个变量自己够不够格被代回」与
                // 「它初值里有没有候选」是两件事，必须分开。
                if let Some(e) = &v.init {
                    collect_forward_candidates_expr(e, decl_counts, out);
                }
                if v.is_static
                    || v.is_extern
                    || v.is_thread_local
                    || v.cleanup_fn.is_some()
                    || v.register_name.is_some()
                    || v.asm_label.is_some()
                    || crate::ast::type_has_volatile(&v.ty)
                    || !is_int_scalar_for_cprop(&v.ty)
                {
                    continue;
                }
                if decl_counts.get(&v.name.name).copied().unwrap_or(0) != 1 {
                    continue;
                }
                out.insert(v.name.name.clone(), v.ty.clone());
            },
            BlockItem::Stmt(s) => collect_forward_candidates_stmt(s, decl_counts, out),
            _ => {},
        }
    }
}

fn collect_forward_candidates_stmt(
    s: &Stmt,
    decl_counts: &HashMap<String, usize>,
    out: &mut HashMap<String, crate::ast::Type>,
) {
    match s {
        Stmt::Block { b, .. } => collect_forward_candidates(&b.items, decl_counts, out),
        Stmt::Expr { e, .. } => collect_forward_candidates_expr(e, decl_counts, out),
        Stmt::Return { val: Some(e), .. } => collect_forward_candidates_expr(e, decl_counts, out),
        Stmt::If {
            cond,
            then_,
            else_,
            ..
        } => {
            collect_forward_candidates_expr(cond, decl_counts, out);
            collect_forward_candidates_stmt(then_, decl_counts, out);
            if let Some(e) = else_ {
                collect_forward_candidates_stmt(e, decl_counts, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            collect_forward_candidates_expr(cond, decl_counts, out);
            collect_forward_candidates_stmt(body, decl_counts, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_forward_candidates_stmt(i, decl_counts, out);
            }
            if let Some(c) = cond {
                collect_forward_candidates_expr(c, decl_counts, out);
            }
            if let Some(st) = step {
                collect_forward_candidates_expr(st, decl_counts, out);
            }
            collect_forward_candidates_stmt(body, decl_counts, out);
        },
        Stmt::Label { stmt, .. } => collect_forward_candidates_stmt(stmt, decl_counts, out),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            collect_forward_candidates_expr(expr, decl_counts, out);
            collect_forward_candidates(prelude, decl_counts, out);
            for c in cases {
                collect_forward_candidates(&c.body.items, decl_counts, out);
            }
        },
        _ => {},
    }
}

fn collect_forward_candidates_expr(
    e: &Expr,
    decl_counts: &HashMap<String, usize>,
    out: &mut HashMap<String, crate::ast::Type>,
) {
    match e {
        Expr::StmtExpr { block, .. } => collect_forward_candidates(&block.items, decl_counts, out),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            collect_forward_candidates_expr(l, decl_counts, out);
            collect_forward_candidates_expr(r, decl_counts, out);
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            collect_forward_candidates_expr(cond, decl_counts, out);
            collect_forward_candidates_expr(then_, decl_counts, out);
            collect_forward_candidates_expr(else_, decl_counts, out);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. } => collect_forward_candidates_expr(x, decl_counts, out),
        Expr::Call { callee, args, .. } => {
            collect_forward_candidates_expr(callee, decl_counts, out);
            for a in args {
                collect_forward_candidates_expr(a, decl_counts, out);
            }
        },
        _ => {},
    }
}

/// 子块递归。两种方向，必须分开：
///   * **裸块 `Stmt::Block`**：没有分支，控制流直落 ⇒ 与父层**共享同一张表**，块尾的
///     `goto` 作为返回值递给父层（现场见 [`forward_items_with`] 的注释）；
///   * 其余形状（`if`/`while`/`for`/`do`/`switch`/`label`/语句表达式）：里面的写可能不执行
///     ⇒ 另起一张表，且不向父层递 goto。
fn forward_nested(
    it: &mut BlockItem,
    qual: &HashMap<String, crate::ast::Type>,
    targets: &FwdAxis,
    lctx: &mut LabelCtx,
    known: &mut HashMap<String, Expr>,
    vals: &mut HashMap<String, i128>,
) -> (bool, Option<String>) {
    if let BlockItem::Stmt(Stmt::Block { b, .. }) = it {
        let mut gi: Option<String> = None;
        // 裸块共享 `known`/`vals` 与标号表 `lctx`：序号是全局单调的，跨层也成立。
        let c = forward_items_with(&mut b.items, qual, targets, lctx, known, vals, &mut gi);
        return (c, gi);
    }
    let c = match it {
        BlockItem::Stmt(s) => forward_nested_stmt(s, qual, targets),
        BlockItem::Decl(Decl::Var(v)) => match &mut v.init {
            Some(e) => forward_nested_expr(e, qual, targets),
            None => false,
        },
        _ => false,
    };
    (c, None)
}

fn forward_nested_stmt(
    s: &mut Stmt,
    qual: &HashMap<String, crate::ast::Type>,
    targets: &FwdAxis,
) -> bool {
    match s {
        Stmt::Block { b, .. } => forward_items(&mut b.items, qual, targets),
        Stmt::If {
            cond,
            then_,
            else_,
            ..
        } => {
            let mut c = forward_nested_expr(cond, qual, targets);
            c |= forward_nested_stmt(then_, qual, targets);
            if let Some(e) = else_ {
                c |= forward_nested_stmt(e, qual, targets);
            }
            c
        },
        Stmt::While { cond, body, .. } => {
            let mut c = forward_nested_expr(cond, qual, targets);
            c |= forward_nested_stmt(body, qual, targets);
            c
        },
        Stmt::Do { body, cond, .. } => {
            let mut c = forward_nested_stmt(body, qual, targets);
            c |= forward_nested_expr(cond, qual, targets);
            c
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            let mut c = false;
            if let Some(i) = init {
                c |= forward_nested_stmt(i, qual, targets);
            }
            if let Some(e) = cond {
                c |= forward_nested_expr(e, qual, targets);
            }
            if let Some(e) = step {
                c |= forward_nested_expr(e, qual, targets);
            }
            c | forward_nested_stmt(body, qual, targets)
        },
        Stmt::Label { stmt, .. } => forward_nested_stmt(stmt, qual, targets),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            let mut c = forward_nested_expr(expr, qual, targets);
            c |= forward_items(prelude, qual, targets);
            for case in cases {
                c |= forward_items(&mut case.body.items, qual, targets);
            }
            c
        },
        _ => false,
    }
}

fn forward_nested_expr(
    e: &mut Expr,
    qual: &HashMap<String, crate::ast::Type>,
    targets: &FwdAxis,
) -> bool {
    match e {
        Expr::StmtExpr { block, .. } => forward_items(&mut block.items, qual, targets),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            let mut c = forward_nested_expr(l, qual, targets);
            c |= forward_nested_expr(r, qual, targets);
            c
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            let mut c = forward_nested_expr(cond, qual, targets);
            c |= forward_nested_expr(then_, qual, targets);
            c | forward_nested_expr(else_, qual, targets)
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. } => forward_nested_expr(x, qual, targets),
        Expr::Call { callee, args, .. } => {
            let mut c = forward_nested_expr(callee, qual, targets);
            for a in args {
                c |= forward_nested_expr(a, qual, targets);
            }
            c
        },
        _ => false,
    }
}

/// 指针局部 → 它指向对象的键（`struct ss_tmp *p = &s;` ⇒ `pins["p"] = "#s"`）。
type Pins = HashMap<String, String>;

/// 左值规范化成一把键：`s.state` ⇒ `#s.state`；`p->state`（`p` 钉到 `&s`）同 ⇒ `#s.state`。
/// 认不出的一律 None（下标、函数返回的指针、取地址后再解引用……）：只有 None 之外的两侧
/// 能对上时，下面那条守卫排除才会生效。
fn lval_key(e: &Expr, pins: &Pins) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(
            pins.get(&name.name)
                .cloned()
                .unwrap_or_else(|| format!("#{}", name.name)),
        ),
        Expr::Member { expr, field, .. } => {
            Some(format!("{}.{}", lval_key(expr, pins)?, field.name))
        },
        // `*p`（`p` 是钉住的指针）就是它指向的那个对象本身，不再加一层 `*`。
        Expr::Deref { e, .. } => {
            if let Expr::Ident { name, .. } = e.as_ref() {
                if let Some(k) = pins.get(&name.name) {
                    return Some(k.clone());
                }
            }
            Some(format!("*{}", lval_key(e, pins)?))
        },
        _ => None,
    }
}

/// 键的根对象（`#s.state` ⇒ `#s`）。
fn key_root(k: &str) -> &str {
    match k.find('.') {
        Some(i) => &k[..i],
        None => k,
    }
}

/// [`build_pins`] 的一遍扫描：候选、`&根` 的取地址次数、名字被写的次数、有无 asm。
#[derive(Default)]
struct PinScan {
    cands: Vec<(String, String)>,
    addr_count: HashMap<String, usize>,
    writes: HashMap<String, usize>,
    asm_seen: bool,
}

impl PinScan {
    fn items(&mut self, items: &[BlockItem]) {
        for it in items {
            match it {
                BlockItem::Decl(Decl::Var(v)) => {
                    if let (Some(Expr::AddrOf { e: target, .. }), crate::ast::Type::Ptr(_)) =
                        (&v.init, &v.ty)
                    {
                        if let Some(k) = lval_key(target, &Pins::new()) {
                            self.cands.push((v.name.name.clone(), k));
                        }
                    }
                    if let Some(e) = &v.init {
                        self.expr(e);
                    }
                },
                BlockItem::Decl(Decl::Func(f)) => {
                    if let Some(b) = &f.body {
                        self.items(&b.items);
                    }
                },
                BlockItem::Stmt(s) => self.stmt(s),
                _ => {},
            }
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block { b, .. } => self.items(&b.items),
            Stmt::Expr { e, .. } => self.expr(e),
            Stmt::Return { val: Some(e), .. } => self.expr(e),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                self.expr(cond);
                self.stmt(then_);
                if let Some(e) = else_ {
                    self.stmt(e);
                }
            },
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                self.expr(cond);
                self.stmt(body);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.stmt(i);
                }
                if let Some(e) = cond {
                    self.expr(e);
                }
                if let Some(e) = step {
                    self.expr(e);
                }
                self.stmt(body);
            },
            Stmt::Label { stmt, .. } => self.stmt(stmt),
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                self.expr(expr);
                for it in prelude {
                    if let BlockItem::Stmt(ss) = it {
                        self.stmt(ss);
                    }
                }
                for c in cases {
                    self.items(&c.body.items);
                }
            },
            Stmt::AsmGoto { .. } => self.asm_seen = true,
            _ => {},
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::AddrOf { e: x, .. } => {
                if let Some(k) = lval_key(x, &Pins::new()) {
                    *self.addr_count.entry(k).or_insert(0) += 1;
                }
                self.expr(x);
                return;
            },
            Expr::Assign { l, r, .. } => {
                if let Expr::Ident { name, .. } = l.as_ref() {
                    *self.writes.entry(name.name.clone()).or_insert(0) += 1;
                }
                self.expr(l);
                self.expr(r);
                return;
            },
            Expr::PreInc { e: x, .. }
            | Expr::PreDec { e: x, .. }
            | Expr::PostInc { e: x, .. }
            | Expr::PostDec { e: x, .. } => {
                if let Expr::Ident { name, .. } = x.as_ref() {
                    *self.writes.entry(name.name.clone()).or_insert(0) += 1;
                }
                self.expr(x);
                return;
            },
            Expr::AsmExpr { outputs, inputs, .. } => {
                self.asm_seen = true;
                for x in outputs.iter().chain(inputs.iter()) {
                    self.expr(x);
                }
                return;
            },
            _ => {},
        }
        // 子表达式递归（只走「可能再出现取地址/写」的那几种）
        match e {
            Expr::Binop { l, r, .. }
            | Expr::Comma { l, r, .. }
            | Expr::Index { arr: l, idx: r, .. } => {
                self.expr(l);
                self.expr(r);
            },
            Expr::Cond {
                cond,
                then_,
                else_,
                ..
            } => {
                self.expr(cond);
                self.expr(then_);
                self.expr(else_);
            },
            Expr::Unary { e: x, .. }
            | Expr::Cast { e: x, .. }
            | Expr::Deref { e: x, .. }
            | Expr::Member { expr: x, .. }
            | Expr::SizeofExpr { e: x, .. }
            | Expr::AlignofExpr { e: x, .. } => self.expr(x),
            Expr::Call { callee, args, .. } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            },
            Expr::StmtExpr { block, .. } => self.items(&block.items),
            Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        InitItem::Expr(x) => self.expr(x),
                        InitItem::Designated { value, .. } => self.expr(value),
                    }
                }
            },
            _ => {},
        }
    }
}

/// 每个函数里哪些指针局部可以被当成「钉住到某个对象」的别名。
///
/// 只有同时满足下面三条才立：初值是 `&<简单左值>`；这个名字在函数里**再没有被写过**；
/// `&根` 出现的次数恰好等于钉到这个根的指针个数（多出来的那几次就是逃逸 —— 交给了调用、
/// 存进了某个指针、进了内联汇编，那些地方可能把它写到别处去）。
fn build_pins(body: &Block) -> Pins {
    let mut scan = PinScan::default();
    scan.items(&body.items);
    let mut out: Pins = HashMap::new();
    for (name, key) in &scan.cands {
        if scan.writes.get(name).copied().unwrap_or(0) > 0 {
            continue;
        }
        out.insert(name.clone(), key.clone());
    }
    // 逃逸判据：`&根` 的出现次数必须被钉住关系全部解释掉。
    //
    // 这一条也覆盖了内联汇编：操作数里的 `&根` 会被 [`PinScan::expr`] 数进去，多出来的
    // 那次就是逃逸 ⇒ 这个根不出事实。反过来，函数里**别处**的 asm（`_s` 根本不沾）
    // 不该一棍子打死所有钉住关系 —— `build_policy.c` 里 `thread_group_cputime()` 就
    // 满是 `__preempt_count` 一类的 asm，旧写法因此把 `__inlN$_sst = &_s` 也拒了。
    let mut pinned: HashMap<String, usize> = HashMap::new();
    for k in out.values() {
        *pinned.entry(key_root(k).to_string()).or_insert(0) += 1;
    }
    out.retain(|_, k| {
        let root = key_root(k).to_string();
        scan.addr_count.get(&root).copied().unwrap_or(0) == pinned.get(&root).copied().unwrap_or(0)
    });
    if std::env::var_os("DCC_GUARD_TRACE").is_some() {
        eprintln!(
            "GUARD| pins={:?} addr_count={:?} writes={:?} asm={}",
            out, scan.addr_count, scan.writes, scan.asm_seen
        );
    }
    out
}

/// `E != K`（K 是整常量）⇒ `(E 的键, K)`。
fn guard_fact(cond: &Expr, pins: &Pins) -> Option<(String, i128)> {
    let Expr::Binop { op: BinOp::Ne, l, r, .. } = cond else {
        return None;
    };
    if let Ok(v) = const_eval::eval(r) {
        let f = (lval_key(l, pins)?, v);
        if std::env::var_os("DCC_GUARD_TRACE").is_some() {
            eprintln!("GUARD| fact {:?}", f);
        }
        return Some(f);
    }
    if let Ok(v) = const_eval::eval(l) {
        return Some((lval_key(r, pins)?, v));
    }
    None
}

/// 臂体是不是以终止语句收尾（后面的臂因此不可能从它落下来）。
fn case_arm_terminates(b: &Block) -> bool {
    let term = |it: Option<&BlockItem>| {
        matches!(
            it,
            Some(BlockItem::Stmt(
                Stmt::Break { .. }
                    | Stmt::Return { .. }
                    | Stmt::Goto { .. }
                    | Stmt::Continue { .. }
            ))
        )
    };
    match b.items.last() {
        Some(BlockItem::Stmt(Stmt::Block { b: inner, .. })) => term(inner.items.last()),
        other => term(other),
    }
}

/// 丢掉被守卫排除的 case 臂。
fn narrow_switch_cases(
    expr: &Expr,
    cases: &mut Vec<crate::ast::Case>,
    facts: &[(String, i128)],
    pins: &Pins,
) -> bool {
    if facts.is_empty() {
        return false;
    }
    let Some(sel) = lval_key(expr, pins) else {
        return false;
    };
    if std::env::var_os("DCC_GUARD_TRACE").is_some() {
        eprintln!("GUARD| switch sel={sel} facts={:?}", facts);
    }
    let mut drop = vec![false; cases.len()];
    let mut any = false;
    for (i, c) in cases.iter().enumerate() {
        let Some(v) = &c.val else { continue };
        if c.range_end.is_some() {
            continue;
        }
        let Ok(k) = const_eval::eval(v) else { continue };
        if !facts.iter().any(|(fk, fv)| fk == &sel && *fv == k) {
            continue;
        }
        // 孤立臂：首臂，或前一臂以终止语句收尾（否则可能从上一臂落下来）。
        if i > 0 && !case_arm_terminates(&cases[i - 1].body) {
            continue;
        }
        // 臂体里有标号 ⇒ 可能有人 `goto` 跳进去。
        if block_has_label(&c.body) {
            continue;
        }
        drop[i] = true;
        any = true;
    }
    if !any {
        return false;
    }
    let mut i = 0;
    cases.retain(|_| {
        let d = drop[i];
        i += 1;
        !d
    });
    true
}

fn narrow_items(items: &mut Vec<BlockItem>, facts: &mut Vec<(String, i128)>, pins: &Pins) -> bool {
    let mut changed = false;
    for it in items.iter_mut() {
        match it {
            BlockItem::Stmt(s) => {
                changed |= narrow_stmt(s, facts, pins);
                facts.retain(|(k, _)| !stmt_may_write_key(s, k, pins));
            },
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &mut v.init {
                    changed |= narrow_expr(e, facts, pins);
                }
                // 声明初值不用再判逃逸：`&根` 出现次数多于「钉住的指针个数」时
                // [`build_pins`] 已经把那个根整个排除在外，根本不会有事实可杀。
            },
            _ => {},
        }
    }
    changed
}

fn narrow_stmt(s: &mut Stmt, facts: &Vec<(String, i128)>, pins: &Pins) -> bool {
    match s {
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            let mut changed = narrow_switch_cases(expr, cases, facts, pins);
            for it in prelude.iter_mut() {
                if let BlockItem::Stmt(ss) = it {
                    changed |= narrow_stmt(ss, facts, pins);
                }
            }
            // 臂体里外面那层事实仍然成立（它们不因选择子改变）。
            let mut inner = facts.clone();
            for c in cases.iter_mut() {
                changed |= narrow_items(&mut c.body.items, &mut inner, pins);
            }
            changed
        },
        Stmt::Block { b, .. } => {
            let mut inner = facts.clone();
            narrow_items(&mut b.items, &mut inner, pins)
        },
        Stmt::If {
            cond,
            then_,
            else_,
            ..
        } => {
            let mut changed = narrow_expr(cond, &mut facts.clone(), pins);
            let mut t = facts.clone();
            if let Some(f) = guard_fact(cond, pins) {
                t.push(f);
            }
            changed |= narrow_stmt(then_, &t, pins);
            if let Some(e) = else_ {
                let ef = facts.clone();
                changed |= narrow_stmt(e, &ef, pins);
            }
            changed
        },
        Stmt::While { cond, body, .. } => {
            let changed = narrow_expr(cond, &mut facts.clone(), pins);
            let mut bf = facts.clone();
            if let Some(f) = guard_fact(cond, pins) {
                bf.push(f);
            }
            changed | narrow_stmt(body, &bf, pins)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            let mut changed = false;
            if let Some(i) = init {
                changed |= narrow_stmt(i, facts, pins);
            }
            let g = match cond {
                Some(c) => {
                    changed |= narrow_expr(c, &mut facts.clone(), pins);
                    guard_fact(c, pins)
                },
                None => None,
            };
            // 体：条件为真才进来，所以事实成立。
            let mut bf = facts.clone();
            if let Some((k, v)) = &g {
                bf.push((k.clone(), *v));
            }
            changed |= narrow_stmt(body, &bf, pins);
            // 步进：条件在**体之前**求值，体里写过 E 的话这里就不成立（C 语义）。
            if let Some(st) = step {
                let mut sf = facts.clone();
                if let Some((k, v)) = &g {
                    if !stmt_may_write_key(body, k, pins) {
                        sf.push((k.clone(), *v));
                    }
                }
                changed |= narrow_expr(st, &mut sf, pins);
            }
            changed
        },
        Stmt::Label { stmt, .. } => narrow_stmt(stmt, facts, pins),
        Stmt::Expr { e, .. } => narrow_expr(e, &mut facts.clone(), pins),
        Stmt::Return { val: Some(e), .. } => narrow_expr(e, &mut facts.clone(), pins),
        _ => false,
    }
}

fn narrow_expr(e: &mut Expr, facts: &mut Vec<(String, i128)>, pins: &Pins) -> bool {
    match e {
        Expr::StmtExpr { block, .. } => narrow_items(&mut block.items, facts, pins),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            let c = narrow_expr(l, facts, pins);
            c | narrow_expr(r, facts, pins)
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            let mut c = narrow_expr(cond, facts, pins);
            c |= narrow_expr(then_, facts, pins);
            c | narrow_expr(else_, facts, pins)
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => narrow_expr(x, facts, pins),
        Expr::Call { callee, args, .. } => {
            let mut c = narrow_expr(callee, facts, pins);
            for a in args {
                c |= narrow_expr(a, facts, pins);
            }
            c
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            let mut c = false;
            for it in inits {
                match it {
                    InitItem::Expr(x) => c |= narrow_expr(x, facts, pins),
                    InitItem::Designated { value, .. } => c |= narrow_expr(value, facts, pins),
                }
            }
            c
        },
        _ => false,
    }
}

/// 本句是否**可能写** `key`，或把这个根交给别人（取地址交给调用/存进指针）。
/// 宁可多答 true（少折一点）；但 false 必须是真的写不到。
fn stmt_may_write_key(s: &Stmt, key: &str, pins: &Pins) -> bool {
    match s {
        Stmt::Expr { e, .. } => expr_may_write_key(e, key, pins),
        Stmt::Return { val, .. } => val
            .as_ref()
            .map(|e| expr_may_write_key(e, key, pins))
            .unwrap_or(false),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_may_write_key(cond, key, pins)
                || stmt_may_write_key(then_, key, pins)
                || else_
                    .as_ref()
                    .map(|e| stmt_may_write_key(e, key, pins))
                    .unwrap_or(false)
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            expr_may_write_key(cond, key, pins) || stmt_may_write_key(body, key, pins)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref()
                .map(|i| stmt_may_write_key(i, key, pins))
                .unwrap_or(false)
                || cond
                    .as_ref()
                    .map(|c| expr_may_write_key(c, key, pins))
                    .unwrap_or(false)
                || step
                    .as_ref()
                    .map(|c| expr_may_write_key(c, key, pins))
                    .unwrap_or(false)
                || stmt_may_write_key(body, key, pins)
        },
        Stmt::Block { b, .. } => block_may_write_key(b, key, pins),
        Stmt::Label { stmt, .. } => stmt_may_write_key(stmt, key, pins),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            expr_may_write_key(expr, key, pins)
                || prelude.iter().any(|it| match it {
                    BlockItem::Stmt(ss) => stmt_may_write_key(ss, key, pins),
                    _ => false,
                })
                || cases.iter().any(|c| block_may_write_key(&c.body, key, pins))
        },
        // 内联汇编的读写集合看不见：一律当「可能写」。
        Stmt::AsmGoto { .. } => true,
        _ => false,
    }
}

fn block_may_write_key(b: &Block, key: &str, pins: &Pins) -> bool {
    b.items.iter().any(|it| match it {
        BlockItem::Stmt(ss) => stmt_may_write_key(ss, key, pins),
        BlockItem::Decl(Decl::Var(v)) => v
            .init
            .as_ref()
            .map(|e| expr_may_write_key(e, key, pins))
            .unwrap_or(false),
        BlockItem::Decl(Decl::Func(f)) => f
            .body
            .as_ref()
            .map(|bb| block_may_write_key(bb, key, pins))
            .unwrap_or(false),
        _ => false,
    })
}

fn expr_may_write_key(e: &Expr, key: &str, pins: &Pins) -> bool {
    let writes_this = |l: &Expr| -> bool {
        match lval_key(l, pins) {
            Some(k) => {
                k == key
                    || key.starts_with(&format!("{k}."))
                    || k.starts_with(&format!("{key}."))
            },
            None => false,
        }
    };
    match e {
        Expr::Assign { l, r, .. } => {
            // 右边把这个根交出去（`p = &s;`、`x.state = …` 除外）也算可能写。
            writes_this(l)
                || expr_may_write_key(l, key, pins)
                || expr_may_write_key(r, key, pins)
                || expr_addr_of_key(r, key, pins)
        },
        Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. } => writes_this(x) || expr_may_write_key(x, key, pins),
        Expr::Call { callee, args, .. } => {
            // 把根（或指向它的指针）交给调用：callee 可能写它。
            args.iter().any(|a| expr_addr_of_key(a, key, pins))
                || expr_addr_of_key(callee, key, pins)
                || args.iter().any(|a| expr_may_write_key(a, key, pins))
                || expr_may_write_key(callee, key, pins)
        },
        Expr::AsmExpr { .. } => true,
        Expr::StmtExpr { block, .. } => block.items.iter().any(|it| match it {
            BlockItem::Stmt(ss) => stmt_may_write_key(ss, key, pins),
            BlockItem::Decl(Decl::Var(v)) => v
                .init
                .as_ref()
                .map(|x| expr_may_write_key(x, key, pins))
                .unwrap_or(false),
            _ => false,
        }),
        Expr::Binop { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            expr_may_write_key(l, key, pins) || expr_may_write_key(r, key, pins)
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            expr_may_write_key(cond, key, pins)
                || expr_may_write_key(then_, key, pins)
                || expr_may_write_key(else_, key, pins)
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => expr_may_write_key(x, key, pins),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            inits.iter().any(|it| match it {
                InitItem::Expr(x) => expr_may_write_key(x, key, pins),
                InitItem::Designated { value, .. } => expr_may_write_key(value, key, pins),
            })
        },
        _ => false,
    }
}

/// 表达式里有没有把这个根「交出去」：`&根`，或一个钉到它的指针被当成值用。
fn expr_addr_of_key(e: &Expr, key: &str, pins: &Pins) -> bool {
    let root = key_root(key);
    let mut hit = false;
    walk_expr_ref(e, &mut |x: &Expr| match x {
        Expr::AddrOf { e: t, .. } => {
            if let Some(k) = lval_key(t, pins) {
                if key_root(&k) == root {
                    hit = true;
                }
            }
        },
        Expr::Ident { name, .. } => {
            if let Some(k) = pins.get(&name.name) {
                if key_root(k) == root {
                    hit = true;
                }
            }
        },
        _ => {},
    });
    hit
}

/// 只读遍历：把 `e` 及其全部子表达式依次交给 `f`。
///
/// **必须穷尽写**（与 `stmt_node_count`/`stmt_max_local_object_bytes` 同一条纪律）：
/// 落进 `_ => {}` 的子表达式在调用方就是「不存在」—— `prune_dead_locals` 据此判
/// 「整函数体一次引用都没有」时，漏一个变体就是把活的声明删掉。实测漏掉
/// `Expr::Real`/`Expr::Imag` 就删掉了 `float _Complex z = …; __real__ z` 里的 `z`
/// （夹具 `vec_cast.c` 的 `check_vector_to_complex` 返回值从 0 变成 7），漏掉
/// `Expr::VaArg`/`Expr::Offsetof` 是同一类。
pub(crate) fn walk_expr_ref<'a>(e: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
    f(e);
    match e {
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            walk_expr_ref(l, f);
            walk_expr_ref(r, f);
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            walk_expr_ref(cond, f);
            walk_expr_ref(then_, f);
            walk_expr_ref(else_, f);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => walk_expr_ref(x, f),
        Expr::Call { callee, args, .. } => {
            walk_expr_ref(callee, f);
            for a in args {
                walk_expr_ref(a, f);
            }
        },
        Expr::AsmExpr { outputs, inputs, .. } => {
            for x in outputs.iter().chain(inputs.iter()) {
                walk_expr_ref(x, f);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => walk_expr_ref(x, f),
                    InitItem::Designated { value, .. } => walk_expr_ref(value, f),
                }
            }
        },
        Expr::StmtExpr { block, .. } => walk_block_ref(block, f),
        // 下面这几条以前全部落进 `_ => {}`：它们的子表达式不是「不需要看」，而是
        // 写的时候漏了。`__real__ z` / `__imag__ z` / `va_arg(ap, T)` / `offsetof(T, a[i])`
        // 都是对局部/变量的真实读取。
        Expr::Real { e: x, .. } | Expr::Imag { e: x, .. } | Expr::VaArg { ap: x, .. } => {
            walk_expr_ref(x, f)
        },
        Expr::Offsetof { indices, .. } => {
            for x in indices {
                walk_expr_ref(x, f);
            }
        },
        // 叶子：没有子表达式。列全而不是靠兜底，新增 `Expr` 变体时这里会直接编不过。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::LabelAddr { .. }
        | Expr::VaArgPack { .. } => {},
    }
}

fn walk_block_ref<'a>(b: &'a Block, f: &mut dyn FnMut(&'a Expr)) {
    for it in &b.items {
        match it {
            BlockItem::Stmt(s) => walk_stmt_ref(s, f),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &v.init {
                    walk_expr_ref(e, f);
                }
            },
            BlockItem::Decl(Decl::Func(ff)) => {
                if let Some(body) = &ff.body {
                    walk_block_ref(body, f);
                }
            },
            _ => {},
        }
    }
}

fn walk_stmt_ref<'a>(s: &'a Stmt, f: &mut dyn FnMut(&'a Expr)) {
    match s {
        Stmt::Block { b, .. } => walk_block_ref(b, f),
        Stmt::Expr { e, .. } => walk_expr_ref(e, f),
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                walk_expr_ref(e, f);
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            walk_expr_ref(cond, f);
            walk_stmt_ref(then_, f);
            if let Some(e) = else_ {
                walk_stmt_ref(e, f);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            walk_expr_ref(cond, f);
            walk_stmt_ref(body, f);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                walk_stmt_ref(i, f);
            }
            if let Some(e) = cond {
                walk_expr_ref(e, f);
            }
            if let Some(e) = step {
                walk_expr_ref(e, f);
            }
            walk_stmt_ref(body, f);
        },
        Stmt::Label { stmt, .. } => walk_stmt_ref(stmt, f),
        // 这两处以前漏了：`asm goto` 的操作数与 `goto *expr` 里的表达式同样是对
        // 局部变量的**真实读取**。内联的形参活跃性判定（[`try_static_inline_general`]
        // 里的死参数裁剪）要是收不到它们，就会把被 asm 读的形参当成死参数、
        // 删掉绑定，产物里就多一条指向未定义名字的重定位。
        Stmt::GotoIndirect { expr, .. } => walk_expr_ref(expr, f),
        Stmt::AsmGoto { outputs, inputs, .. } => {
            for x in outputs.iter().chain(inputs.iter()) {
                walk_expr_ref(x, f);
            }
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            walk_expr_ref(expr, f);
            // `prelude` 里的**声明初始化式**以前没走：`switch (…) { int t = x; case 0: … }`
            // 的 `x` 是对局部的一次真实读取，漏了它「从未被引用」就成了假事实
            // （[`prune_dead_locals`] 会据此把 `x` 的声明删掉）。
            for it in prelude {
                match it {
                    BlockItem::Stmt(ss) => walk_stmt_ref(ss, f),
                    BlockItem::Decl(Decl::Var(v)) => {
                        if let Some(e) = &v.init {
                            walk_expr_ref(e, f);
                        }
                    },
                    _ => {},
                }
            }
            for c in cases {
                walk_block_ref(&c.body, f);
            }
        },
        // C++ 的两条语句同样可能读局部（`invoke` 的调用、landing pad 的两个操作数）。
        // 以前落进 `_ => {}`：对活名字是漏计，对「从未被引用」是假事实。
        Stmt::Invoke { call, .. } => walk_stmt_ref(call, f),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            walk_expr_ref(object, f);
            walk_expr_ref(selector, f);
        },
        _ => {},
    }
}

/// 循环/分支条件上的排除值窄化（详见 [`narrow_switch_cases`]）。
fn narrow_switch_by_excluded_guard(module: &mut Module) -> bool {
    let prog = module.source_mut();
    let mut changed = false;
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let Some(body) = &mut f.body else { continue };
            let pins = build_pins(body);
            let mut facts: Vec<(String, i128)> = Vec::new();
            changed |= narrow_items(&mut body.items, &mut facts, &pins);
        }
    }
    changed
}

/// 把一项认成「`x = <整常量>`」：左边必须是裸名字，右边必须折成一个整常量。
///
/// 复合赋值（`+=` 等）与别的一切写法都不算存储 —— 它们在 `Expr::Assign` 里同形，
/// 靠 `op` 分辨（`op == BinOp::Add` 就是普通 `=`，判据与 `collect_cprop_disq_expr` 同源）。
fn store_const_stmt(it: &BlockItem) -> Option<(String, i128, crate::ast::Type, Span)> {
    let BlockItem::Stmt(Stmt::Expr { e, .. }) = it else {
        return None;
    };
    let Expr::Assign { op, l, r, .. } = e else {
        return None;
    };
    if !matches!(op, crate::ast::BinOp::Add) {
        return None;
    }
    let Expr::Ident { name, ty, span } = l.as_ref() else {
        return None;
    };
    let v = store_value_ctx(r)?;
    Some((name.name.clone(), v, ty.clone(), span.clone()))
}

/// 「这条 store 到底存进去什么」：与 `const_eval::eval_full` 的差别只有赋值表达式一臂。
///
/// `eval_full` 的 `Assign` 臂是给**全局初始化器**用的宽松口径（直接返回原始 RHS，
/// 副作用留给运行期执行），而值语境里赋值表达式的值是「存入之后的对象值」
/// （C11 6.5.16p3）—— 每多一层窄化就多一次截断。按宽松口径把 `u = uc = 300` 记成
/// `u = 300` 之后，`u != 44` 被折成真（`tests/fixtures/c/chained_assign_truncation.c`
/// 的 -O1/-O2/-O3 实测 abort，-O0 正常）。
///
/// 嵌在别的写法里的赋值（`x = (a = 1) + 2`）同样会被宽松口径错算，那一形交给
/// [`crate::semantics::discarded_value_needs_code`]：它一路下探，遇到赋值/调用/volatile 读
/// 就答「要代码」，而这里的语义正是「不求值就不能算常量」。
fn store_value_ctx(e: &Expr) -> Option<i128> {
    if let Expr::Assign {
        op, l, r, span, ..
    } = e
    {
        // 复合赋值（`x += 3`）的值是旧值与 RHS 运算后的结果；`eval_full` 的 `Assign`
        // 臂连 `op` 都不看，不能借它的答案。
        if !matches!(op, crate::ast::BinOp::Add) {
            return None;
        }
        // 位域成员的写值还要按**声明宽度**再截一次，而 AST 的 `Expr::Member` 只带
        // 结构体字段类型、不带位宽（`unsigned x : 3` 与普通 `unsigned x` 同型）
        // ⇒ 这一形算不出真值，宁可放弃：`long r = (b.x = 300u);` 真值是 4，按 300
        // 代回去 `ssa_assign_value` 的 narrow 立刻错（实测）。非位域成员跟着一起
        // 放弃只是少折一条，方向保守。
        if matches!(l.as_ref(), Expr::Member { .. }) {
            return None;
        }
        let v = store_value_ctx(r)?;
        // 转换语义不另写一份：把值包成一次 `Cast` 交给 `const_eval`，截断/符号扩展
        // 与别处的 `(T)` 同源（`uc = 300` ⇒ 44，`sc = 200` ⇒ -56）。
        let conv = Expr::Cast {
            ty: l.ty(),
            e: Box::new(Expr::Int {
                v,
                span: span.clone(),
                ty: l.ty(),
            }),
            span: span.clone(),
        };
        return const_eval::eval(&conv).ok();
    }
    if crate::semantics::discarded_value_needs_code(e) {
        return None;
    }
    const_eval::eval(e).ok()
}

/// 一个 `ty` 类型的对象用 `e` 初始化/赋值之后，**对象里真正躺着的值**。
///
/// 与 [`store_value_ctx`] 的分工：那个只算「存进去的原始值」，转换交给调用方按需做；
/// 这里把 C11 6.7.9p11 的「初始化 = 一次赋值转换」一起做完，供**候选常量表**用 ——
/// 表里的值会被当成「任何时刻读这个名字都得到它」，差一位就是静默错值：
///   * `long r = (*dp = 300);`（`dp` 是 `i8 *`）的对象值是 44，不是 300；
///   * `unsigned int y = 1;` 的对象值是 `1U`，不是 `1`（按 `1` 代进 `x > y` 会把
///     比较退回有符号，`mixed_signedness_usual_arith` 的 prop_* 四条 -O1 实测）。
///
/// `_Bool` 单独按 C11 6.3.1.2 归一成 0/1：`const_eval` 的 `Cast` 对 `Bool` 走的是
/// 8 位无符号掩码（`(_Bool)256` 会得 0），而 C 里任何非零值都存成 1。
fn store_value_as(e: &Expr, ty: &Type, span: &Span) -> Option<i128> {
    let v = store_value_ctx(e)?;
    if matches!(ty, Type::Bool) {
        return Some((v != 0) as i128);
    }
    if matches!(ty, Type::Float | Type::Double | Type::LongDouble | Type::Float128) {
        return None;
    }
    let conv = Expr::Cast {
        ty: ty.clone(),
        e: Box::new(Expr::Int {
            v,
            span: span.clone(),
            ty: ty.clone(),
        }),
        span: span.clone(),
    };
    const_eval::eval(&conv).ok()
}

/// 收候选（顶层 + 嵌套块）。安全边界三条，缺一条宁可漏收：
///  1. 名字在整个函数里只声明一次，且不是形参（`decl_counts` / `param_names`）；
///  2. 声明所在的项列表里没有任何标号 —— 有标号就可能被 `goto` 跳进去绕过初始化。
///     内联自造的出口标号不算（`stmt_has_label` 已经把 `__inl<N>__…__end` 当非标号）；
///  3. `switch` 的 `prelude` 与 `case` 体这两层不收：跳 `case` 标号能跳过前面声明的
///     初始化，而 `case` 标号不在 `items` 里、第 2 条看不见。往这两层里面的普通块继续下探。
///
/// 「嵌套块里的同名局部遮蔽外层/全局同名对象」不靠这里判：代回那侧按 C 作用域走
/// （`subst_cprop_items`），只在自己被声明的项列表里代。
fn collect_cprop_candidates(
    items: &[BlockItem],
    decl_counts: &HashMap<String, usize>,
    param_names: &std::collections::HashSet<String>,
    collect_here: bool,
    fn_top: bool,
    out: &mut HashMap<String, Expr>,
) {
    // 函数体顶层保持旧口径：那里的局部对整个函数体可见，而且带标号的函数体极常见
    // （错误路径的 `goto out;`），一并拒掉会把老覆盖面全退回去。嵌套块没这个待遇：
    // 那里的声明可能被 `goto` 跳过去绕过初始化。
    if collect_here && (fn_top || !items_have_label(items)) {
        for it in items {
            let BlockItem::Decl(Decl::Var(v)) = it else { continue };
            if v.is_static
                || v.is_extern
                || v.is_thread_local
                || v.cleanup_fn.is_some()
                || v.register_name.is_some()
                || v.asm_label.is_some()
                || crate::ast::type_has_volatile(&v.ty)
                || !is_int_scalar_for_cprop(&v.ty)
            {
                continue;
            }
            let name = v.name.name.clone();
            if std::env::var_os("DCC_CPROP_TRACE").is_some() {
                eprintln!(
                    "[cprop] 局部 {name} 声明次数={} 是形参={} 整型标量={} 初值可折={}",
                    decl_counts.get(&name).copied().unwrap_or(0),
                    param_names.contains(&name),
                    is_int_scalar_for_cprop(&v.ty),
                    const_foldable_init(v).is_some(),
                );
            }
            if decl_counts.get(&name).copied().unwrap_or(0) != 1 || param_names.contains(&name) {
                continue;
            }
            let Some(init) = const_foldable_init(v) else { continue };
            // 候选值必须是「初始化执行完之后对象里**真躺着**的值」，不是初值表达式的
            // 原始值：`long r = (*dp = 300);`（`dp` 是 `i8 *`）的对象值是 44，按 300
            // 代回会让 `return r;` 返回 300 —— 实测 `ssa_assign_value` 的 narrow、
            // `nested_vla_upvar_capture` 的 v 只在 -O1 红（-O2/-O3 的 SSA 重建会按左值
            // 重新载入，把错盖住），定性后才回到这里。值与类型一起按**声明类型**取：
            // `unsigned int y = 1;` 的对象值是 `1U`，按字面量自带的 `int` 代进
            // `x > y` 会折成有符号比较（`mixed_signedness_usual_arith` 的 prop_* 四条）。
            // 也不能只认「已经折叠成字面量」的那一种：负常量在 AST 里是 `Unary{Neg,Int}`，
            // 内核 `clamp_val(v, -20, 19)` 的 `lo` 正是这个形状。
            let Some(c) = store_value_as(&init, &v.ty, &v.span) else { continue };
            out.entry(name).or_insert(Expr::Int {
                v: c,
                span: v.span.clone(),
                ty: v.ty.clone(),
            });
        }
    }
    for it in items {
        match it {
            BlockItem::Stmt(s) => {
                collect_cprop_candidates_stmt(s, decl_counts, param_names, out)
            },
            // 初始化式里的语句表达式是主力客户：`BUILD_BUG_ON` 所在的 `({ … })`
            // 常常就是某个局部（`addr`）的初值。
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &v.init {
                    collect_cprop_candidates_expr(e, decl_counts, param_names, out);
                }
            },
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &f.body {
                    collect_cprop_candidates(&b.items, decl_counts, param_names, true, true, out);
                }
            },
            _ => {},
        }
    }
}

fn collect_cprop_candidates_stmt(
    s: &Stmt,
    decl_counts: &HashMap<String, usize>,
    param_names: &std::collections::HashSet<String>,
    out: &mut HashMap<String, Expr>,
) {
    match s {
        Stmt::Block { b, .. } => {
            collect_cprop_candidates(&b.items, decl_counts, param_names, true, false, out)
        },
        Stmt::Expr { e, .. } => collect_cprop_candidates_expr(e, decl_counts, param_names, out),
        Stmt::Return { val: Some(e), .. } => {
            collect_cprop_candidates_expr(e, decl_counts, param_names, out)
        },
        Stmt::If { cond, then_, else_, .. } => {
            collect_cprop_candidates_expr(cond, decl_counts, param_names, out);
            collect_cprop_candidates_stmt(then_, decl_counts, param_names, out);
            if let Some(el) = else_ {
                collect_cprop_candidates_stmt(el, decl_counts, param_names, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            collect_cprop_candidates_expr(cond, decl_counts, param_names, out);
            collect_cprop_candidates_stmt(body, decl_counts, param_names, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_cprop_candidates_stmt(i, decl_counts, param_names, out);
            }
            if let Some(c) = cond {
                collect_cprop_candidates_expr(c, decl_counts, param_names, out);
            }
            if let Some(st) = step {
                collect_cprop_candidates_expr(st, decl_counts, param_names, out);
            }
            collect_cprop_candidates_stmt(body, decl_counts, param_names, out);
        },
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            collect_cprop_candidates_expr(expr, decl_counts, param_names, out);
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    collect_cprop_candidates_stmt(st, decl_counts, param_names, out);
                }
            }
            for c in cases {
                for it in &c.body.items {
                    if let BlockItem::Stmt(st) = it {
                        collect_cprop_candidates_stmt(st, decl_counts, param_names, out);
                    }
                }
            }
        },
        Stmt::Label { stmt, .. } => {
            collect_cprop_candidates_stmt(stmt, decl_counts, param_names, out)
        },
        _ => {},
    }
}

/// 表达式里的候选：主要为钻进**语句表达式**（`({ … })`）。内核里 `BUILD_BUG_ON`
/// 大量坐在这种块里（`apic_mem_write` 那条 `({ … })` 把 `fix_to_virt` 里的断言整段
/// 装了进来），不下探就一条也折不动。块里按普通块收（同样过标号而拒）。
fn collect_cprop_candidates_expr(
    e: &Expr,
    decl_counts: &HashMap<String, usize>,
    param_names: &std::collections::HashSet<String>,
    out: &mut HashMap<String, Expr>,
) {
    match e {
        Expr::StmtExpr { block, .. } => {
            collect_cprop_candidates(&block.items, decl_counts, param_names, true, false, out)
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_cprop_candidates_expr(l, decl_counts, param_names, out);
            collect_cprop_candidates_expr(r, decl_counts, param_names, out);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::VaArg { ap: x, .. } => {
            collect_cprop_candidates_expr(x, decl_counts, param_names, out)
        },
        Expr::Call { callee, args, .. } => {
            collect_cprop_candidates_expr(callee, decl_counts, param_names, out);
            for a in args {
                collect_cprop_candidates_expr(a, decl_counts, param_names, out);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_cprop_candidates_expr(cond, decl_counts, param_names, out);
            collect_cprop_candidates_expr(then_, decl_counts, param_names, out);
            collect_cprop_candidates_expr(else_, decl_counts, param_names, out);
        },
        Expr::Index { arr, idx, .. } => {
            collect_cprop_candidates_expr(arr, decl_counts, param_names, out);
            collect_cprop_candidates_expr(idx, decl_counts, param_names, out);
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            for op in inputs.iter().chain(outputs.iter()) {
                collect_cprop_candidates_expr(op, decl_counts, param_names, out);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => {
                        collect_cprop_candidates_expr(x, decl_counts, param_names, out)
                    },
                    InitItem::Designated { value, .. } => {
                        collect_cprop_candidates_expr(value, decl_counts, param_names, out)
                    },
                }
            }
        },
        _ => {},
    }
}

fn is_int_scalar_for_cprop(ty: &crate::ast::Type) -> bool {
    matches!(
        ty,
        crate::ast::Type::Bool
            // 枚举行必须算：内核那一族 `switch (level) { … default: BUILD_BUG(); }`
            // 的形参就是 `enum pgtable_level`，内联后临时量 `__inlN$_level` 也是枚举型。
            // 不认枚举 ⇒ 选择子代不回去 ⇒ 常量 switch 折不动 ⇒ `default:` 臂里的
            // `BUILD_BUG()` 留在产物里（实测 `mm/memory.o` 7 条、`mm/rmap.o` 4 条、
            // `kernel/signal.o` 7 条 `__compiletime_assert_NNN`；五秒夹具 `/tmp/kwsw2/sw.c`）。
            | crate::ast::Type::Enum(..)
            | crate::ast::Type::Char
            | crate::ast::Type::SChar
            | crate::ast::Type::UChar
            | crate::ast::Type::Short
            | crate::ast::Type::UShort
            | crate::ast::Type::Int
            | crate::ast::Type::UInt
            | crate::ast::Type::Long
            | crate::ast::Type::ULong
            | crate::ast::Type::LongLong
            | crate::ast::Type::ULongLong
    )
}

fn count_var_decls(items: &[BlockItem], out: &mut HashMap<String, usize>) {
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => {
                *out.entry(v.name.name.clone()).or_insert(0) += 1;
                // 初始化式里可能藏着语句表达式块（内联展开后的形参临时量常在里面），
                // 不进去数就会把它们算成「声明 0 次」而拒掉。
                if let Some(e) = &v.init {
                    count_var_decls_expr(e, out);
                }
            },
            BlockItem::Decl(Decl::Func(ff)) => {
                if let Some(b) = &ff.body {
                    count_var_decls(&b.items, out);
                }
            },
            BlockItem::Stmt(s) => count_var_decls_stmt(s, out),
            _ => {},
        }
    }
}

fn count_var_decls_stmt(s: &Stmt, out: &mut HashMap<String, usize>) {
    match s {
        Stmt::Block { b, .. } => count_var_decls(&b.items, out),
        Stmt::Expr { e, .. } => count_var_decls_expr(e, out),
        Stmt::Return { val: Some(e), .. } => count_var_decls_expr(e, out),
        Stmt::If { cond, then_, else_, .. } => {
            count_var_decls_expr(cond, out);
            count_var_decls_stmt(then_, out);
            if let Some(e) = else_ {
                count_var_decls_stmt(e, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            count_var_decls_expr(cond, out);
            count_var_decls_stmt(body, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                count_var_decls_stmt(i, out);
            }
            if let Some(c) = cond {
                count_var_decls_expr(c, out);
            }
            if let Some(st) = step {
                count_var_decls_expr(st, out);
            }
            count_var_decls_stmt(body, out);
        },
        Stmt::Switch {
            expr, cases, prelude, ..
        } => {
            count_var_decls_expr(expr, out);
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    count_var_decls_stmt(st, out);
                }
            }
            for c in cases {
                count_var_decls(&c.body.items, out);
            }
        },
        Stmt::Label { stmt, .. } => count_var_decls_stmt(stmt, out),
        _ => {},
    }
}

fn count_var_decls_expr(e: &Expr, out: &mut HashMap<String, usize>) {
    match e {
        Expr::StmtExpr { block, .. } => count_var_decls(&block.items, out),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            count_var_decls_expr(l, out);
            count_var_decls_expr(r, out);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::VaArg { ap: x, .. } => count_var_decls_expr(x, out),
        Expr::Call { callee, args, .. } => {
            count_var_decls_expr(callee, out);
            for a in args {
                count_var_decls_expr(a, out);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            count_var_decls_expr(cond, out);
            count_var_decls_expr(then_, out);
            count_var_decls_expr(else_, out);
        },
        Expr::Index { arr, idx, .. } => {
            count_var_decls_expr(arr, out);
            count_var_decls_expr(idx, out);
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            for op in inputs.iter().chain(outputs.iter()) {
                count_var_decls_expr(op, out);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => count_var_decls_expr(x, out),
                    InitItem::Designated { value, .. } => count_var_decls_expr(value, out),
                }
            }
        },
        _ => {},
    }
}

/// 收「不能让这个局部当常量」的事实：取址、asm 操作数里的名字、`++`/`--` 目标，
/// 以及「赋成了不同的常量」。与 `collect_cand_writes_*` 同口径，但那边不区分
/// 「普通的 `x = 常量`」与「`x = 其他`」，而本判据要允前者。
fn collect_cprop_disqualifiers(
    items: &[BlockItem],
    consts: &HashMap<String, i128>,
    out: &mut std::collections::HashSet<String>,
    hazards_only: bool,
) {
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &v.init {
                    collect_cprop_disq_expr(e, consts, out, hazards_only);
                }
            },
            BlockItem::Stmt(s) => collect_cprop_disq_stmt(s, consts, out, hazards_only),
            _ => {},
        }
    }
}

/// `hazards_only`：只收「地址逃逸 / asm 操作数 / `++`/`--` 」这三类**与写值无关**的
/// 全局危害，不看「被赋了别的值」。
///
/// 为什么要这个模式：本判据有两组消费者，它们要的事实不同。
///   * `propagate_one_func` 要的是「任何时刻读 x 都是 C」，所以「被赋过别的值」也算死（`false`）。
///   * `propagate_const_stores_forward` 是**逐条语句**问「这一条有没有动过 x」，
///     「被写」由它自己按位置处理（写之前代回、写之后作废）；但它**不能**容忍
///     x 的地址在任何地方逃逸过 —— 那时代回值可能已被别人改掉。这里就要 `true`。
/// 两个模式共用一张臂表，不另起第二套判据（本仓的 walker 不一致已经出过几次问题）。
fn collect_cprop_disq_stmt(
    s: &Stmt,
    consts: &HashMap<String, i128>,
    out: &mut std::collections::HashSet<String>,
    hazards_only: bool,
) {
    match s {
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            collect_cprop_disq_expr(e, consts, out, hazards_only)
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            collect_cprop_disq_expr(cond, consts, out, hazards_only);
            collect_cprop_disq_stmt(then_, consts, out, hazards_only);
            if let Some(el) = else_ {
                collect_cprop_disq_stmt(el, consts, out, hazards_only);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            collect_cprop_disq_expr(cond, consts, out, hazards_only);
            collect_cprop_disq_stmt(body, consts, out, hazards_only);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_cprop_disq_stmt(i, consts, out, hazards_only);
            }
            if let Some(c) = cond {
                collect_cprop_disq_expr(c, consts, out, hazards_only);
            }
            if let Some(st) = step {
                collect_cprop_disq_expr(st, consts, out, hazards_only);
            }
            collect_cprop_disq_stmt(body, consts, out, hazards_only);
        },
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            collect_cprop_disq_expr(expr, consts, out, hazards_only);
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    collect_cprop_disq_stmt(st, consts, out, hazards_only);
                }
            }
            for c in cases {
                collect_cprop_disqualifiers(&c.body.items, consts, out, hazards_only);
            }
        },
        Stmt::Block { b, .. } => {
            collect_cprop_disqualifiers(&b.items, consts, out, hazards_only)
        },
        Stmt::Label { stmt, .. } => collect_cprop_disq_stmt(stmt, consts, out, hazards_only),
        Stmt::AsmGoto {
            outputs,
            inputs,
            input_constraints,
            ..
        } => {
            for o in outputs {
                mark_all_idents(o, consts, out);
            }
            for (i, c) in inputs.iter().zip(input_constraints.iter()) {
                // 「非要立即数不可」的约束 + 操作数是**裸 Ident** ⇒ 这一次访问只是把值当
                // 字面量灌进模板（jump-label 的 `JUMP_TABLE_ENTRY("%c0 + %c1", …)` 里那个
                // `branch`），既不写它也不取它地址 ⇒ 不该判死。以前 inputs 一律 mark，
                // 内联造的临时量因此进不了候选表（实测 `FWD| fn=use hazard=["__inl2$_branch"]`），
                // 代回根本无从发生，最后编码期报 E0311。
                // 其它形状照旧判死：`"i"(&x)`、`"i"(a[3])` 这类是把地址交给 asm。
                let bare_ident = matches!(i, Expr::Ident { .. });
                if bare_ident
                    && crate::codegen::shared::asm_constraints::constraint_is_immediate_only(c)
                {
                    continue;
                }
                mark_all_idents(i, consts, out);
            }
        },
        _ => {},
    }
}

fn collect_cprop_disq_expr(
    e: &Expr,
    consts: &HashMap<String, i128>,
    out: &mut std::collections::HashSet<String>,
    hazards_only: bool,
) {
    let mark_root = |x: &Expr, out: &mut std::collections::HashSet<String>| {
        if let Expr::Ident { name, .. } = x {
            if consts.contains_key(&name.name) {
                out.insert(name.name.clone());
            }
        }
    };
    match e {
        Expr::Assign { op, l, r, .. } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if let Some(c) = consts.get(&name.name) {
                    // `x = <同一个常量>` 不会弄坏不变式；其他一律判死（包括复合赋值）
                    let ok = matches!(op, crate::ast::BinOp::Add)
                        && match const_eval::eval_full(r, false) {
                            Ok(crate::const_eval::ConstValue::Int(v)) => v == *c,
                            _ => false,
                        };
                    if !ok && !hazards_only {
                        out.insert(name.name.clone());
                    }
                }
            }
            collect_cprop_disq_expr(l, consts, out, hazards_only);
            collect_cprop_disq_expr(r, consts, out, hazards_only);
        },
        Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. } => {
            mark_root(x, out);
            collect_cprop_disq_expr(x, consts, out, hazards_only);
        },
        Expr::AddrOf { e: x, .. } => {
            mark_root(x, out);
            collect_cprop_disq_expr(x, consts, out, hazards_only);
        },
        // 初始化器里同样可能取址：`int *arr[3] = {&a, &b, &c};` 若看不见这三个 `&`，
        // `a/b/c` 就留在候选表里，代回器随后把 `&a` 代成 `&1`（-O1 实测：数组里存的是
        // 1/2/3，`*arr[0]` 当场 SIGSEGV；-O0 正常）。以前这一形整棵落进 `_ => {}`。
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => collect_cprop_disq_expr(x, consts, out, hazards_only),
                    InitItem::Designated { value, .. } => {
                        collect_cprop_disq_expr(value, consts, out, hazards_only)
                    },
                }
            }
        },
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_cprop_disq_expr(l, consts, out, hazards_only);
            collect_cprop_disq_expr(r, consts, out, hazards_only);
        },
        Expr::Call { callee, args, .. } => {
            collect_cprop_disq_expr(callee, consts, out, hazards_only);
            for a in args {
                collect_cprop_disq_expr(a, consts, out, hazards_only);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_cprop_disq_expr(cond, consts, out, hazards_only);
            collect_cprop_disq_expr(then_, consts, out, hazards_only);
            collect_cprop_disq_expr(else_, consts, out, hazards_only);
        },
        Expr::Index { arr, idx, .. } => {
            collect_cprop_disq_expr(arr, consts, out, hazards_only);
            collect_cprop_disq_expr(idx, consts, out, hazards_only);
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::VaArg { ap: x, .. } => collect_cprop_disq_expr(x, consts, out, hazards_only),
        Expr::AsmExpr { outputs, inputs, .. } => {
            for o in outputs {
                mark_all_idents(o, consts, out);
            }
            for i in inputs {
                mark_all_idents(i, consts, out);
            }
        },
        Expr::StmtExpr { block, .. } => {
            collect_cprop_disqualifiers(&block.items, consts, out, hazards_only)
        },
        _ => {},
    }
}

/// asm 的操作数：子树里出现的**任何**候选名字都判死（内存约束不发 `&`，取址计数看不见）。
fn mark_all_idents(
    e: &Expr,
    consts: &HashMap<String, i128>,
    out: &mut std::collections::HashSet<String>,
) {
    match e {
        Expr::Ident { name, .. } => {
            if consts.contains_key(&name.name) {
                out.insert(name.name.clone());
            }
        },
        Expr::Member { expr, .. }
        | Expr::Index { arr: expr, .. }
        | Expr::AddrOf { e: expr, .. }
        | Expr::Deref { e: expr, .. }
        | Expr::Cast { e: expr, .. }
        | Expr::Unary { e: expr, .. } => mark_all_idents(expr, consts, out),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            mark_all_idents(l, consts, out);
            mark_all_idents(r, consts, out);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            mark_all_idents(cond, consts, out);
            mark_all_idents(then_, consts, out);
            mark_all_idents(else_, consts, out);
        },
        Expr::Call { callee, args, .. } => {
            mark_all_idents(callee, consts, out);
            for a in args {
                mark_all_idents(a, consts, out);
            }
        },
        _ => {},
    }
}

/// 只改语句的条件与 switch 选择子（不碰赋值右边、不碰普通表达式位置），返回是否改过。
///
/// 作用域按 C 规则走：候选只在自己被声明的那个项列表里、且只在其声明之后可见。
/// `visible` 是外层已经可见的那部分（嵌套块继承）；本层再声明同名局部时以本层为准。
fn subst_cprop_items(
    items: &mut [BlockItem],
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
) -> bool {
    let mut scope = visible.clone();
    let mut changed = false;
    for it in items.iter_mut() {
        match it {
            BlockItem::Stmt(s) => changed |= subst_cprop_stmt(s, cands, &scope, clean),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &mut v.init {
                    changed |= subst_cprop_in_expr(e, cands, &scope, clean);
                }
                // 候选到这里才进作用域：初始化式自己读同名 (int x = x;) 不能代成新值。
                if let Some(c) = cands.get(&v.name.name) {
                    scope.insert(v.name.name.clone(), c.clone());
                }
            },
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    // GNU 嵌套函数自成作用域，外面可见的候选也不往外层带。
                    let empty: HashMap<String, Expr> = HashMap::new();
                    changed |= subst_cprop_items(&mut b.items, cands, &empty, clean);
                }
            },
            _ => {},
        }
    }
    changed
}

fn subst_cprop_in_expr(
    e: &mut Expr,
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
) -> bool {
    // 返回值只用于「有没有可能改过」的粗粒度判断：位置里只要有可见候选的读就当成改过，
    // 真正的等值性由入选条件保证，不在这里比字符串。
    let touched = expr_mentions_any(e, visible);
    subst_cprop_expr_scoped(e, cands, visible, clean);
    touched
}

/// `subst_cand_reads_expr` 的作用域版：多一条 `StmtExpr` 臂 —— 代回器本体（内联那边
/// 也在用）故意不下探语句表达式，而本 pass 必须进去：内核 `BUILD_BUG_ON` 大量坐在
/// `({ … })` 里，块内条件读的又是块内刚声明的形参临时量，不按块代就永远折不动。
///
/// 未逐个列出的表达式型别交给 `subst_cand_reads_expr` 兜底（它按 `visible` 整棵代一遍，
/// 集合内都是可见候选，不会代错，只是可能漏掉更深层的语句表达式）。
fn subst_cprop_expr_scoped(
    e: &mut Expr,
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
) {
    match e {
        Expr::Ident { .. } => subst_cand_reads_expr(e, visible, clean),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            subst_cprop_expr_scoped(l, cands, visible, clean);
            subst_cprop_expr_scoped(r, cands, visible, clean);
        },
        // `&` 的操作数不是一次**读**，而是左值指示符：把候选常量代进 `&a` 只会
        // 得到 `&1`。见 [`subst_cprop_designator`]。
        Expr::AddrOf { e: x, .. } => subst_cprop_designator(x, cands, visible, clean),
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::VaArg { ap: x, .. } => subst_cprop_expr_scoped(x, cands, visible, clean),
        Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => subst_cprop_expr_scoped(x, cands, visible, clean),
        Expr::Call { callee, args, .. } => {
            subst_cprop_expr_scoped(callee, cands, visible, clean);
            // `__builtin_constant_p` 的操作是**语法查询**，不是一次求值：
            // 代回候选值等于把问题从「这个表达式是不是常量」换成「这个值是不是常量」。
            if !is_syntactic_query_callee(callee) {
                for a in args {
                    subst_cprop_expr_scoped(a, cands, visible, clean);
                }
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            subst_cprop_expr_scoped(cond, cands, visible, clean);
            subst_cprop_expr_scoped(then_, cands, visible, clean);
            subst_cprop_expr_scoped(else_, cands, visible, clean);
        },
        Expr::Index { arr, idx, .. } => {
            subst_cprop_expr_scoped(arr, cands, visible, clean);
            subst_cprop_expr_scoped(idx, cands, visible, clean);
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            for op in inputs.iter_mut().chain(outputs.iter_mut()) {
                subst_cprop_expr_scoped(op, cands, visible, clean);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits.iter_mut() {
                match it {
                    InitItem::Expr(x) => subst_cprop_expr_scoped(x, cands, visible, clean),
                    InitItem::Designated { value, .. } => {
                        subst_cprop_expr_scoped(value, cands, visible, clean)
                    },
                }
            }
        },
        Expr::StmtExpr { block, .. } => {
            subst_cprop_items(&mut block.items, cands, visible, clean);
        },
        _ => subst_cand_reads_expr(e, visible, clean),
    }
}

/// `__builtin_constant_p` 的操作数**不求值**，只问「这个表达式在语法上是不是常量」
/// （`sizeof` 同理，但那里代回不改变答案，所以只在这里闸）。代回器一律不进它的实参：
///
/// ```text
/// int x = 1;
/// if (__builtin_constant_p(x) != 0) return 2;   /* -O0..-O3 都必须答 0 */
/// ```
///
/// 不闸的后果（实测 `tests/fixtures/c/constp.c`）：`propagate_const_locals_in_conditions`
/// 把 `x` 的声明初值代进实参，长成 `__builtin_constant_p(1)`，紧接着 `fold_constants`
/// 折成 1 ⇒ -O1/-O2/-O3 返回 2 而 -O0 返回 0，`scripts/check_opt_equivalence.sh`
/// 的「四档等价」当场变红。GCC 自己对这个问题的答案也会随 `-O` 变，
/// 但本仓的契约是**档位无关**（那个脚本就是这条契约的判据）。
fn is_syntactic_query_callee(callee: &Expr) -> bool {
    matches!(callee, Expr::Ident { name, .. } if name.name == "__builtin_constant_p")
}

/// `&` 的操作数位置：左值**指示符**，不是值读。这里只下探「求值的那个位置」
/// （下标、`*p` 的 `p`），裸名字一律留着。
///
/// 现场（-O1 实测，`tests/fixtures/c/pointer.c`）：
/// ```text
/// int a = 1, b = 2, c = 3;
/// int *arr[3] = {&a, &b, &c};   /* 存进数组的成了 1/2/3 */
/// *arr[0]                        /* 解引用 1 当场 SIGSEGV */
/// ```
/// 根因是 [`collect_cprop_disq_expr`] 的判据没下探初始化器（见那里的 `InitList` 臂）
/// ⇒ 「取过址」这件事没被记上 ⇒ `a` 留在候选表里。修好那一处之后这里仍要保留：
/// 代回器不该依赖「上游一定没漏」，`&x` 里的 `x` 本来就不是一次读。
fn subst_cprop_designator(
    e: &mut Expr,
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
) {
    match e {
        Expr::Ident { .. } => {},
        // `&*p`：`p` 自己是个值，照常代。
        Expr::Deref { e: x, .. } => subst_cprop_expr_scoped(x, cands, visible, clean),
        Expr::Index { arr, idx, .. } => {
            subst_cprop_designator(arr, cands, visible, clean);
            subst_cprop_expr_scoped(idx, cands, visible, clean);
        },
        Expr::Member { expr, .. } => subst_cprop_designator(expr, cands, visible, clean),
        // 其余形状（语句表达式、复合字面量……）拿不准就不动。
        _ => {},
    }
}

/// 这条表达式（含子树、含语句表达式的块体、含初始化器）里是否出现了候选名字。
fn expr_mentions_any(e: &Expr, cands: &HashMap<String, Expr>) -> bool {
    expr_mentions_name(e, &|n| cands.contains_key(n))
}

/// `expr_mentions_any` 的谓词版：哪些名字算「命中」交给调用方定（常量候选表、
/// 或是「被调方的形参/局部名单」）。
fn expr_mentions_name(e: &Expr, hit: &dyn Fn(&str) -> bool) -> bool {
    match e {
        Expr::Ident { name, .. } => hit(&name.name),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            expr_mentions_name(l, hit) || expr_mentions_name(r, hit)
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::VaArg { ap: x, .. } => expr_mentions_name(x, hit),
        Expr::Index { arr, idx, .. } => {
            expr_mentions_name(arr, hit) || expr_mentions_name(idx, hit)
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_mentions_name(cond, hit)
                || expr_mentions_name(then_, hit)
                || expr_mentions_name(else_, hit)
        },
        Expr::Call { callee, args, .. } => {
            expr_mentions_name(callee, hit) || args.iter().any(|a| expr_mentions_name(a, hit))
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            inputs.iter().any(|o| expr_mentions_name(o, hit))
                || outputs.iter().any(|o| expr_mentions_name(o, hit))
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            inits.iter().any(|it| match it {
                InitItem::Expr(x) => expr_mentions_name(x, hit),
                InitItem::Designated { value, .. } => expr_mentions_name(value, hit),
            })
        },
        Expr::StmtExpr { block, .. } => block
            .items
            .iter()
            .any(|it| item_mentions_name(it, hit)),
        _ => false,
    }
}

fn item_mentions_name(it: &BlockItem, hit: &dyn Fn(&str) -> bool) -> bool {
    match it {
        BlockItem::Decl(Decl::Var(v)) => {
            v.init.as_ref().map(|e| expr_mentions_name(e, hit)).unwrap_or(false)
        },
        BlockItem::Decl(Decl::Func(f)) => f
            .body
            .as_ref()
            .map(|b| b.items.iter().any(|i| item_mentions_name(i, hit)))
            .unwrap_or(false),
        BlockItem::Stmt(s) => stmt_mentions_name(s, hit),
        _ => false,
    }
}

fn stmt_mentions_name(s: &Stmt, hit: &dyn Fn(&str) -> bool) -> bool {
    match s {
        Stmt::Block { b, .. } => b.items.iter().any(|i| item_mentions_name(i, hit)),
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => expr_mentions_name(e, hit),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_mentions_name(cond, hit)
                || stmt_mentions_name(then_, hit)
                || else_.as_ref().map(|e| stmt_mentions_name(e, hit)).unwrap_or(false)
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_mentions_name(cond, hit) || stmt_mentions_name(body, hit)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().map(|i| stmt_mentions_name(i, hit)).unwrap_or(false)
                || cond.as_ref().map(|c| expr_mentions_name(c, hit)).unwrap_or(false)
                || step.as_ref().map(|c| expr_mentions_name(c, hit)).unwrap_or(false)
                || stmt_mentions_name(body, hit)
        },
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            expr_mentions_name(expr, hit)
                || prelude.iter().any(|i| item_mentions_name(i, hit))
                || cases
                    .iter()
                    .any(|c| c.body.items.iter().any(|i| item_mentions_name(i, hit)))
        },
        Stmt::Label { stmt, .. } => stmt_mentions_name(stmt, hit),
        // **`asm goto` 的操作数也是「提到」**。漏了这条，`drop_dead_inline_temps` 会把
        // 只在 `"i"(形参临时量)` 里出现的声明当死代码删掉 —— 删的是声明，留下的是
        // 指向它的 asm goto，codegen 只能报
        // `E0311: impossible constraint ... (expression: __inlN$_key)`。
        // 实测 `kernel/sched/fair.c`：`static_branch_likely(&sched_feat_keys[…]` 一族
        // （26 条）全部死在这一步，`reweight_task_fair` 的 AST 里只剩操作数、没有声明。
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            inputs.iter().any(|o| expr_mentions_name(o, hit))
                || outputs.iter().any(|o| expr_mentions_name(o, hit))
        },
        Stmt::GotoIndirect { expr, .. } => expr_mentions_name(expr, hit),
        _ => false,
    }
}

fn subst_cprop_stmt(
    s: &mut Stmt,
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
) -> bool {
    let mut changed = false;
    match s {
        Stmt::If { cond, then_, else_, .. } => {
            changed |= subst_cprop_in_expr(cond, cands, visible, clean);
            changed |= subst_cprop_stmt(then_, cands, visible, clean);
            if let Some(el) = else_ {
                changed |= subst_cprop_stmt(el, cands, visible, clean);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            changed |= subst_cprop_in_expr(cond, cands, visible, clean);
            changed |= subst_cprop_stmt(body, cands, visible, clean);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                changed |= subst_cprop_stmt(i, cands, visible, clean);
            }
            if let Some(c) = cond {
                changed |= subst_cprop_in_expr(c, cands, visible, clean);
            }
            if let Some(st) = step {
                changed |= subst_cprop_in_expr(st, cands, visible, clean);
            }
            changed |= subst_cprop_stmt(body, cands, visible, clean);
        },
        Stmt::Switch {
            expr, prelude, cases, ..
        } => {
            changed |= subst_cprop_in_expr(expr, cands, visible, clean);
            // prelude 里声明的候选不外泄到 case 体：跳 `case` 标号能跳过它的初始化。
            changed |= subst_cprop_items(prelude, cands, visible, clean);
            for c in cases.iter_mut() {
                changed |= subst_cprop_items(&mut c.body.items, cands, visible, clean);
            }
        },
        Stmt::Block { b, .. } => changed |= subst_cprop_items(&mut b.items, cands, visible, clean),
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            changed |= subst_cprop_into_stmt_exprs(e, cands, visible, clean)
        },
        Stmt::Label { stmt, .. } => changed |= subst_cprop_stmt(stmt, cands, visible, clean),
        // asm 语句（含 `asm goto`）的**输入**操作数也是对局部名的真实读取，以前整臂
        // 漏了：内联把 `static_key_false(arch_static_branch(key, false))` 逐层展开后，
        // 操作数停在临时量 `__inlN$_branch` 上，编码期拿不到立即数，直接
        // `error[E0311]: impossible constraint in 'asm' … (expression: __inl2$_branch,
        // in function use)`（站点 `arch/x86/include/asm/jump_label.h:37` 的
        // `JUMP_TABLE_ENTRY("%c0 + %c1", "%l[l_yes]")`）—— jump-label 全内核在用，
        // 不代这一条就是整批 TU 编不过。
        //
        // 两道限制：① 只代 `inputs`，不代 `outputs` —— 输出/读写操作数是 asm 要**写**的
        // 地方，换成常量等于把写入目标变成一个立即数；② 只在约束「非要立即数不可」时代，
        // 判据沿用 `asm_constraints::constraint_is_immediate_only`（与上面
        // `Expr::AsmExpr` 那一臂、以及内联器的操作数裁剪共用同一份口径），寄存器/内存约束
        // 保持原样，不改变它们的编码路径。
        Stmt::AsmGoto {
            inputs,
            input_constraints,
            ..
        } => {
            for (op, c) in inputs.iter_mut().zip(input_constraints.iter()) {
                if crate::codegen::shared::asm_constraints::constraint_is_immediate_only(c) {
                    // 只在真的会换东西时报 changed（`subst_cprop_expr_scoped` 不回报结果，
                    // 拿它当“已改”会把上层迭代当成无限收敛信号）。
                    let will = matches!(&*op, Expr::Ident { name, .. }
                        if cands.contains_key(&name.name) && !clean.contains(&name.name));
                    subst_cprop_expr_scoped(op, cands, visible, clean);
                    changed |= will;
                }
            }
        },
        _ => {},
    }
    changed
}

/// 只下探到**语句表达式**块与**赋值右值**为止：块里按项/作用域正常走
/// （`subst_cprop_items`），块外只有「即将被存进左值的值」会被代成常量，其余值位置
/// （`return x;`、`f(x);` ……）一个字符也不改 —— 本 pass 的职责是「把常量送进存储，
/// 让位置轴接下去」，不是通用常量传播。
///
/// 为什么值位置必须挡住（这一条是实测打回来的）：本轮修好候选值模型（[`store_value_as`]）
/// 之前，`long r1 = (*dp = 300);` 的候选值按宽松口径算成 300（真值 44），而语句值位置的
/// 读一被代回，`return r1;` 就静默返回 300 —— `ssa_assign_value` 的 narrow 与
/// `nested_vla_upvar_capture` 的 v 只在 -O1 红（-O2/-O3 的 SSA 重建按左值重新载入，
/// 把错盖住）。值模型修好后这条不再产生错值，但该位置本来就不在职责内：多代只会让
/// 产物无谓漂移，且每多一处值位置（位域、volatile、左值转换）就多一条要单独论证的格。
///
/// 赋值右值是唯一例外，而且必须保留：`r1 = r2;`（r2 已知常量）代成 `r1 = 0;` 之后
/// 位置轴才看得见这条存储，`if (r1)` 才能折 —— 内核那族「只有声明没有定义」的编译期
/// 陷阱函数就靠这一步清掉（`iommu_amdv1.o` 的 `__pt_no_sw_bit`、`net/ethtool/ioctl.o`
/// 等 6 处 `____wrong_branch_error`）。
///
/// 为什么要进语句表达式：`BUILD_BUG_ON` 所在的 `({ … })` 常常只是某条表达式语句/返回值
/// 的一部分（`r = fv(514);`、`return fv(514);`），不进去就永远折不动那条断言的死分支。
fn subst_cprop_into_stmt_exprs(
    e: &mut Expr,
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
) -> bool {
    subst_cprop_values(e, cands, visible, clean, false)
}

/// [`subst_cprop_into_stmt_exprs`] 的本体。`reads` 只在**赋值右值**里为真：那里换掉
/// 一个已知常量是等值改写，而且正是位置轴要的「常量存储」；其余位置只负责往下找
/// 语句表达式块。
fn subst_cprop_values(
    e: &mut Expr,
    cands: &HashMap<String, Expr>,
    visible: &HashMap<String, Expr>,
    clean: &std::collections::HashSet<String>,
    reads: bool,
) -> bool {
    match e {
        Expr::StmtExpr { block, .. } => subst_cprop_items(&mut block.items, cands, visible, clean),
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            subst_cprop_values(l, cands, visible, clean, reads)
                | subst_cprop_values(r, cands, visible, clean, reads)
        },
        // **赋值只代源位，不代目标位**：目标位是「要往哪儿写」，把它换成常量就是把
        // 写入目标改成立即数。而源位本来就是「这次要存进去的值」—— 代成已知常量后
        // 位置轴（`propagate_const_stores_forward`）才能把名字当常量读。
        // 实测代价（`if (0) { r2 = 1; goto e; } else { r2 = 0; goto e; } e: ; r1 = r2;
        // if (r1) 陷阱;`，最小件 `.work_tmp/repro_cprop_across_label.c`）：函数里有
        // `goto`+标号就进不了 SSA/SCCP 的回写通道，cprop 是唯一的代回机会，代不动 ⇒
        // 恒假支塌不掉 ⇒ 内核那族「只有声明没有定义」的编译期陷阱函数活到链接期。
        Expr::Assign { r, .. } => subst_cprop_values(r, cands, visible, clean, true),
        Expr::Ident { name, .. } => {
            if reads && visible.contains_key(&name.name) && !clean.contains(&name.name) {
                subst_cand_reads_expr(e, visible, clean);
                return true;
            }
            false
        },
        // `&x` 是左值指示符不是读：代成 `&常量` 就错了（与
        // [`subst_cprop_designator`] 同一口径）。
        Expr::AddrOf { e: x, .. } => {
            if reads {
                subst_cprop_designator(x, cands, visible, clean);
                false
            } else {
                subst_cprop_values(x, cands, visible, clean, false)
            }
        },
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::VaArg { ap: x, .. } => subst_cprop_values(x, cands, visible, clean, reads),
        Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => subst_cprop_values(x, cands, visible, clean, reads),
        Expr::Call { callee, args, .. } => {
            let mut ch = subst_cprop_values(callee, cands, visible, clean, reads);
            for a in args {
                ch |= subst_cprop_values(a, cands, visible, clean, reads);
            }
            ch
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            subst_cprop_values(cond, cands, visible, clean, reads)
                | subst_cprop_values(then_, cands, visible, clean, reads)
                | subst_cprop_values(else_, cands, visible, clean, reads)
        },
        Expr::Index { arr, idx, .. } => {
            subst_cprop_values(arr, cands, visible, clean, reads)
                | subst_cprop_values(idx, cands, visible, clean, reads)
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            let mut ch = false;
            for op in inputs.iter_mut().chain(outputs.iter_mut()) {
                ch |= subst_cprop_values(op, cands, visible, clean, reads);
            }
            ch
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            let mut ch = false;
            for it in inits.iter_mut() {
                match it {
                    InitItem::Expr(x) => {
                        ch |= subst_cprop_values(x, cands, visible, clean, reads)
                    },
                    InitItem::Designated { value, .. } => {
                        ch |= subst_cprop_values(value, cands, visible, clean, reads)
                    },
                }
            }
            ch
        },
        _ => false,
    }
}

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
    // `forced_only()` 会把整张函数表**深拷贝**一份（每个 `FuncDecl` 的函数体 AST + 全部字符串）。
    // 它以前挂在下面「每个 caller」的循环里 ⇒ `tcp_input.c` 这种几千函数、单体上万节点的 TU
    // 是平方级深拷贝（实测该 TU 25 分钟不出产物，perf 热点 60% + 在
    // `malloc`/`free`/`memmove`/`sip::Hasher::write` 上，都是这里在反复建表）。
    // 表只跟 `table` 有关、与 caller 无关，所以循环外建一次。
    let forced_table = if inline_walk_enabled() {
        Some(table.forced_only())
    } else {
        None
    };
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            let caller_flatten = f.is_flatten;
            let caller_is_hot = f.is_hot;
            if let Some(body) = &mut f.body {
                // 判决行只带 callee 名，而同一 callee 可能在十几个 caller 里展开；
                // 报障时要能把每条 INLINE 归到具体 caller（同一个 caller 的判决是连着的）。
                if inline_trace_enabled() {
                    eprintln!("CALLER| {}", f.name.name);
                }
                // 闸门关着时这一条通道什么都不管，不必再付一次全树量尺与站点收集。
                let walk_arms = inline_walk_enabled() && arms_walkable(body);
                if inline_trace_enabled() && inline_walk_enabled() {
                    // 预算的归因行：调格阵时要知道「哪个 caller 被哪条线挡住」，而不只是
                    // 产物变了。`walk_arms` 直接打真实判决，**不在这里重算公式**：上一版
                    // 就是在这重算少了一个 `threaded_channel_enabled()`，默认档恒报
                    // 「walk_arms=true」，会把「通道没开」读成「开了但没收益」。
                    let nodes = arm_body_nodes(body);
                    eprintln!(
                        "BUDGET| func={} nodes={nodes} limit={} threaded={} walk_arms={}",
                        f.name.name,
                        arm_budget_limit(),
                        has_threaded_dispatch(body),
                        walk_arms
                    );
                }
                inline_calls_in_block(
                    body,
                    &table,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    0,
                    walk_arms,
                );
                // 臂体预算闸门关掉时（`nodes` 超上限：实测 `intel_pmu_init` 6109 对 800），
                // 上面那趟整体不进 `switch`/`Label` 臂体，于是臂里的 `__always_inline`
                // 调用点一字未动 —— 而它是**语义要求**，不是「要不要顺便内联」的成本判断。
                // 残留调用点会让 DCE 保留 callee 定义，定义体里的 `"i"(形参)` 直接编不出来。
                // 这里补一趟只带强制内联函数的遍历：表里没别的函数，成本模型全部落空，
                // 真正被展开的只剩 `__always_inline`。
                // `flatten` 调用者要求体内所有调用都展开，那一档只能用完整表。
                if !walk_arms && inline_walk_enabled() {
                    let sub = match (&forced_table, caller_flatten) {
                        (Some(forced), false) => forced,
                        _ => &table,
                    };
                    inline_calls_in_block(
                        body,
                        sub,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        0,
                        true,
                    );
                }
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
        // 类型定义与 `asm` 块不含可展开的调用点（`GlobalAsm` 的文本里可能的调用今天
        // 就不在内联器视野里），按叶子计 1。列全而不是 `_ => 1`：`Decl` 新加变体时必须
        // 在这里显式表态（与 `src/ast_dispatch.rs` 的 walker 同一条纪律）。
        BlockItem::Decl(Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_)) => 1,
        BlockItem::Decl(Decl::Typedef { .. } | Decl::GlobalAsm { .. }) => 1,
    }
}

/// 语句体的节点数。**必须穷尽且递归到所有子节点**：它同时是成本模型的输入与
/// 「臂体展开预算」（[`arm_budget_ok_for`]）的量尺。评审实测过教训：以前这里带一个
/// `_ => 1`，于是 `Stmt::Label`（Lua 的 `vmcase(l)` 展开就是一个标签）、`GotoIndirect`、
/// `Invoke`、`EhLandingPad`、`AsmGoto` 全部被当成 1 个节点，`luaV_execute` 量出来只有
/// ~250 节点（真实上万）⇒ 预算在标签形语料上完全失效（fail-open）。
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
        // `prelude` 也是体的一部分：`switch (x) { int y = f(); case 0: ... }` 里那段不在
        // `cases` 下，漏计它等于给预算开一个绕过口。
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            1 + expr_node_count(expr)
                + prelude.iter().map(item_node_count).sum::<usize>()
                + cases
                    .iter()
                    .map(|c| {
                        // `case` 的值与 GNU 区间上界也算节点（与 `ast_dispatch.rs::walk_case`
                        // 同一口径）：它们必须是常量表达式，不计不会让预算错开，但会让
                        // 「穷尽且递归」这句自述与实际不符。
                        1 + c.val.as_ref().map_or(0, expr_node_count)
                            + c.range_end.as_ref().map_or(0, expr_node_count)
                            + c.body.items.iter().map(item_node_count).sum::<usize>()
                    })
                    .sum::<usize>()
        },
        // 标签把整段体括在 `stmt` 里（Lua 的 `vmcase` 就是这个形状），必须递归；以前
        // 它只计 1，预算对标签形完全失效。
        Stmt::Label { stmt, .. } => 1 + stmt_node_count(stmt),
        Stmt::GotoIndirect { expr, .. } => 1 + expr_node_count(expr),
        Stmt::Invoke { call, .. } => 1 + stmt_node_count(call),
        Stmt::EhLandingPad {
            object, selector, ..
        } => 1 + expr_node_count(object) + expr_node_count(selector),
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            1 + outputs
                .iter()
                .chain(inputs.iter())
                .map(expr_node_count)
                .sum::<usize>()
        },
        Stmt::Goto { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => 1,
    }
}

fn init_node_count(init: &crate::ast::InitItem) -> usize {
    match init {
        crate::ast::InitItem::Expr(e) => expr_node_count(e),
        crate::ast::InitItem::Designated { value, .. } => expr_node_count(value),
    }
}

/// 投机内联的「大局部对象」闸门：callee 体里**最大的单个局部对象**超过这个字节数
/// 就不内联。
///
/// 这个数是 gcc 的 `--param large-stack-frame=256` 默认值；gcc 在
/// `can_inline_edge_p` 里对**所有**投机内联用它（与调用点个数无关），因为它防的
/// 是栈帧，不是代码体积。
///
/// 为什么必须补这一条（报告 §5.39）：`init/main.c::initcall_blacklisted` 里有一个
/// `char fn_name[KSYM_SYMBOL_LEN]`（本配置 672 B），dcc 把它投机内联进
/// `do_one_initcall` ⇒ 后者的帧从 gcc 的 80 B 变成 1 792 B。16 KB 内核栈上，
/// early-boot 那条 25 层 initcall 链（`kernel_init` → `do_one_initcall` →
/// `net_dev_init` → … → `_raw_spin_trylock`，首段帧合计 11 088 B）因此打穿 guard page。
///
/// 判据只看**单个对象**而不是「所有局部之和」：数组与聚合一定要一块连续帧内存，
/// 而标量可以拿到寄存器家（`ssa::regalloc::deliver`），把它们加进去会把一批本来就
/// 很小的函数误判成大帧。漏判的方向是「仍然内联」= 今天的行为，不会引入新错值。
const INLINE_LARGE_LOCAL_BYTES: usize = 256;

/// 一个局部声明会占多少字节帧（向上圆整到 8）。`static`/`extern` 不占本帧。
fn local_decl_frame_bytes(v: &VarDecl) -> usize {
    if v.is_static || v.is_extern {
        return 0;
    }
    let n = v.ty.size().max(8);
    (n + 7) & !7
}

/// 一个项列表里**最大的单个局部对象**字节数（不递归进嵌套函数：它有自己的帧）。
fn max_local_object_bytes(items: &[BlockItem]) -> usize {
    let mut m = 0usize;
    for it in items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => m = m.max(local_decl_frame_bytes(v)),
            BlockItem::Stmt(s) => m = m.max(stmt_max_local_object_bytes(s)),
            // 嵌套函数的局部住在它自己的帧里，与本次内联无关。
            BlockItem::Decl(_) => {},
        }
    }
    m
}

/// 语句子树里最大的单个局部对象字节数。穷举写（与 [`stmt_node_count`] 同一条纪律）：
/// 新增 `Stmt` 变体时这里必须表态，否则大数组可以沿新语法漏过闸门。
/// 表达式里只有语句表达式 `({ ... })` 能带声明，其余不递归（那里不会有大数组）。
fn stmt_max_local_object_bytes(s: &Stmt) -> usize {
    match s {
        Stmt::Return { .. }
        | Stmt::Expr { .. }
        | Stmt::GotoIndirect { .. }
        | Stmt::Goto { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => 0,
        Stmt::If {
            then_, else_, ..
        } => stmt_max_local_object_bytes(then_).max(else_.as_ref().map_or(0, |e| {
            stmt_max_local_object_bytes(e)
        })),
        Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_max_local_object_bytes(body),
        Stmt::For { init, body, .. } => init
            .as_ref()
            .map_or(0, |s| stmt_max_local_object_bytes(s))
            .max(stmt_max_local_object_bytes(body)),
        Stmt::Block { b, .. } => max_local_object_bytes(&b.items),
        Stmt::Switch { cases, prelude, .. } => {
            let mut m = max_local_object_bytes(prelude);
            for c in cases {
                m = m.max(max_local_object_bytes(&c.body.items));
            }
            m
        },
        Stmt::Label { stmt, .. } => stmt_max_local_object_bytes(stmt),
        Stmt::Invoke { call, .. } => stmt_max_local_object_bytes(call),
        Stmt::EhLandingPad { .. } | Stmt::AsmGoto { .. } => 0,
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
    walk_arms: bool,
) {
    inline_calls_in_items(
        &mut body.items,
        funcs,
        level,
        caller_flatten,
        caller_is_hot,
        fenv_access,
        depth,
        walk_arms,
    );
}

/// 一个「项列表」层面的内联遍历。拆出来是为了让 `switch` 的 `prelude` 与块体走**同一套**
/// 分派：评审实测到 prelude 自己重写了一份 match（只认 `Stmt` 与嵌套 `Decl::Func`），
/// 于是 `switch (x) { int y = f(); case 0: … }` 里那个初值调用点不会被处理，而同一个形状
/// 在普通块里会（那一支受 `DCC_INLINE_SHAPE` 管，今天不可见，但两份规则已经开始漂）。
fn inline_calls_in_items(
    items: &mut Vec<BlockItem>,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
    walk_arms: bool,
) {
    // 「声明初值」展开后的语句暂存在这里（下标, 待插入项），逆序插回：边遍历边
    // Vec::insert 与 iter_mut() 不能共存。
    let mut expanded_inits: Vec<(usize, BlockItem)> = Vec::new();
    for (i, item) in items.iter_mut().enumerate() {
        match item {
            BlockItem::Stmt(s) => inline_calls_in_stmt(
                s,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
                walk_arms,
            ),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b) = &mut f.body {
                    // 嵌在词法里的函数是一个**独立的 caller**：预算按它自己的体量算（沿外层
                    // caller 的判决会连坐一个只有十几个节点的嵌套函数，反过来也会让超上限的
                    // 外层替它「预先批准」）。
                    inline_calls_in_block(
                        b,
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth,
                        arms_walkable(b),
                    );
                }
            },
            BlockItem::Decl(Decl::Var(v)) => {
                // 第 4 类形状（方案 §4 批 3）：`T x = f(...)` 的声明初值，受
                // DCC_INLINE_SHAPE 管。展开机器只认语句形状，所以把它等价改写成
                // 「原位置留一个无初值声明 + 紧跟一条 x = f(...)」再交给同一条通道：
                // 变量还在自己的作域与位置上（生命周期、同名遮蔽、帧槽登记都不变），
                // 变的那一条赋值与 `x = f(...)` 逐字同路。不能把展开块包进嵌套块作用域
                // —— 那会把声明关进子里层，后面的使用点成了未解析名字，而本 pass 跑在
                // 语义分析之后：codegen 会把它懒注册成另一个栈槽（静默错值那一族）。
                //
                // 闸门只管「成本模型这条路」，不管 `__always_inline`：后者的兜底在下面
                // 那条 `inline_expr` 上（形状更宽，初值/初始化列表/嵌套位置都覆盖）。
                if inline_shape_enabled() && level >= 2 {
                    // 只动自动存储期的普通局部：static/extern/TLS 的初值是**加载期**的事实
                    // （搬进作用域内的语句就变成每次执行），带 VLA/清理函数/寄存器绑定/
                    // 别名/asm label 的声明也不是「存个值」那么简单。
                    let plain_local = !v.is_static
                        && !v.is_extern
                        && !v.is_thread_local
                        && v.cleanup_fn.is_none()
                        && v.register_name.is_none()
                        && v.alias_name.is_none()
                        && v.asm_label.is_none()
                        && !type_has_vla_dim(&v.ty);
                    if plain_local && matches!(v.init, Some(Expr::Call { .. })) {
                        let init = v.init.take().expect("上面判过 Some");
                        let mut synth = Stmt::Expr {
                            e: Expr::Assign {
                                op: BinOp::Add,
                                l: Box::new(Expr::Ident {
                                    name: v.name.clone(),
                                    span: v.span.clone(),
                                    ty: v.ty.clone(),
                                }),
                                r: Box::new(init.clone()),
                                span: v.span.clone(),
                                ty: v.ty.clone(),
                            },
                            span: v.span.clone(),
                        };
                        // 走完整的语句通道（专属两条与通用成本路都可能在这条语句上接住），
                        // 所以「有没有展开」要比对语句本身，不能只看某一个返回值。
                        let before = format!("{:?}", synth);
                        inline_calls_in_stmt(
                            &mut synth,
                            funcs,
                            level,
                            caller_flatten,
                            caller_is_hot,
                            fenv_access,
                            depth,
                            walk_arms,
                        );
                        if format!("{:?}", synth) == before {
                            // 什么也没接住：初值原样放回去，这一条与改动前逐字节相同。
                            v.init = Some(init);
                        } else {
                            expanded_inits.push((i + 1, BlockItem::Stmt(synth)));
                        }
                    }
                }
                // `__always_inline` 兜底（与上面的闸门无关）：初值里还留着这种调用点就在
                // **原位**展开。不走上面那条「改成赋值语句」的重写（那条受性能闸门管），
                // 而是把初值表达式交给 `inline_expr`——它对「体不是单 return」的强制内联
                // 调用会在原位生成 `({ ...; __rv; })`，在初始化器/初始化列表里都是合法
                // 表达式，于是 `struct S x = { .f = rip_rel_ptr(&g) }` 这种内核写法也能展开。
                if let Some(init) = v.init.take() {
                    if expr_has_forced_inline_call(&init, funcs, caller_flatten)
                        || expr_has_discarded_comma_call(&init, funcs)
                    {
                        v.init = Some(inline_expr(
                            init,
                            funcs,
                            level,
                            caller_flatten,
                            caller_is_hot,
                            fenv_access,
                            depth,
                        ));
                    } else {
                        v.init = Some(init);
                    }
                }
            },
            // 类型定义与 `asm` 声明里没有可展开的调用点（`GlobalAsm` 的文本本来就不在
            // 内联器视野里）。列全而不是 `_ => {}`：`Decl` 新加变体时必须在这里显式表态
            // （与上面的 [`item_node_count`] 、`src/ast_dispatch.rs` 同一条纪律）。
            BlockItem::Decl(Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_)) => {},
            BlockItem::Decl(Decl::Typedef { .. } | Decl::GlobalAsm { .. }) => {},
        }
    }
    // 逆序插回：先插靠后的，前面那些待插下标才不会被撑位。
    for (at, it) in expanded_inits.into_iter().rev() {
        items.insert(at, it);
    }
}

/// 循环条件里的强制内联闸门（`DCC_INLINE_LOOP_COND`，默认**开**）。理由见
/// [`inline_calls_in_stmt`] 的 `Stmt::While`/`Stmt::Do` 两条臂注释。
fn inline_loop_cond_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| parse_gate("DCC_INLINE_LOOP_COND", true))
}

/// 布尔闸门共用的一个解析（三个 `DCC_INLINE_*` 闸门都走它）。
///
/// 为什么不能只认那四个词、其余一律当「关」：`DCC_INLINE_WALK=TRUE` 会静默变成关侧，
/// 而关侧正是配对 A/B 的**基准臂** —— 一个拼错的大小写就能把「开」的样本混进「关」的
/// 里去定结论。现在：明确的是/否都认，认不了就**警一行 + 按该闸门的默认值**（警不进产物，
/// 不坏可复现：同一 env 同一产物）。
fn parse_gate(name: &str, default: bool) -> bool {
    let raw = match std::env::var(name) {
        Ok(v) => v,
        Err(_) => return default,
    };
    let t = raw.trim().to_ascii_lowercase();
    match t.as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        "" => default,
        other => {
            eprintln!(
                "dcc: 警告: {name}={other} 不是 1/0/true/false/on/off，已按默认值 {default} 处理"
            );
            default
        },
    }
}

/// 内联「调用点形状」这一族的归因闸门（方案 §4 批 3 点名的 `DCC_INLINE_SHAPE`）。
/// 它只管本轮新接的两类：`x op= f(...)`（脱糖形态）与**声明初值** `int x = f(...)`。
/// 已实测过的三类（`x = f()` / `y = x = f()` / `if (f())`）不在它名下 —— 那三类的默认值
/// 已经由上一轮 15 轮配测定为开（median(r)=0.9514，SQLite 边际 −4.9%），把它们一起关掉
/// 会把那个已经记账的结论抹掉。与 `blockopt::level()` 同一纪律：`OnceLock` 只读一次 env。
///
/// **默认关**（本轮实测，与批 2 那三条同处置）：机制与判据都在，但同轮配对量出来是
/// 负的 —— Lua `fib(32)` `median(r)=1.0188`（CI [0.9987,1.0351]，4/15 轮更快），
/// SQLite 20 万行负载 `median(r)=1.0135`（CI [0.9965,1.0240]）；而 SQLite 那个与开臂
/// **同一二进制**的 dup 臂自己报 1.0212 ⇒ 两个同二进制臂方向一致地慢，这不是位置效应。
/// 代价的另一面是体量：Lua 二进制 +53KB（1.14MB→1.19MB）、sqlite3.o +346KB（7.69MB→8.03MB）。
/// 结构原因也说得清：这两类的展开比 `x = f()` 多一个「结果临时量 + 再存回」的固定开销，
/// 而 `estimate_inline_cost` 只看 callee 体与实参，看不到这份 per-site 开销 —— 把它计入
/// 成本模型（或按调用点数量限制这两类）才能重开。注意这与 `DCC_INLINE_WALK` 的展开预算
/// **不是同一条待办**：预算已经做完并把 WALK 翻成默认开，而这两类的回退与 caller 体量无关
/// （体量代价只有 +53KB/+346KB，远碰不到预算那一档的内存墙）。
fn inline_shape_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| parse_gate("DCC_INLINE_SHAPE", false))
}

/// 表达式里是否还留着「必须展开」的调用点：callee 是 `__always_inline`，或 caller 自己
/// 带 `flatten` 属性（那种 caller 的每个调用点都必须展开）。
///
/// 为什么要单独判它，而不是直接拿 [`inline_shape_enabled`] 当条件：那个闸门的默认关是
/// **性能**结论（声明初值/脱糖赋值两类展开使得 Lua `fib(32)` 慢 1.9%、SQLite 慢 1.4%），
/// 而 `__always_inline` 是**语义**要求。不展开的代价不是「多一次调用」而是「编译器还得
/// 另发一份独立副本」，而内核里带 `"i"(形参)` 的 always_inline 函数（`asm/asm.h` 的
/// `rip_rel_ptr`、`cpufeature.h` 的 `_static_cpu_has`）副本本身根本编不出来——实测报
/// `impossible constraint in 'asm' ... (expression: p)`（`arch/x86/boot/startup/gdt_idt.c`）。
/// 表达式里是否存在「逗号左侧的 static 调用」——值被丢弃、可按语句展开。
///
/// 只看形状不看成本：是否真的展开由 [`try_static_inline_general`] 的成本模型决定。
/// 这条谓词的用途只有一个：声明初值路径平时**不**走 [`inline_expr`]（那条只给
/// `__always_inline` 兜底），而 `T x = (chk(...), expr);` 这种内核宏产物就卡在这条
/// 谓词上（现场见 [`inline_expr`] 的 `Expr::Comma` 臂）。
fn expr_has_discarded_comma_call(e: &Expr, funcs: &InlineTable) -> bool {
    let mut found = false;
    walk_expr_ref(e, &mut |x| {
        if found {
            return;
        }
        if let Expr::Comma { l, .. } = x {
            if let Expr::Call { callee, .. } = l.as_ref() {
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    if funcs
                        .get(&name.name)
                        .is_some_and(|f| f.is_static && !f.is_noinline)
                    {
                        found = true;
                    }
                }
            }
        }
    });
    found
}

fn expr_has_forced_inline_call(e: &Expr, funcs: &InlineTable, caller_flatten: bool) -> bool {
    let sub = |x: &Expr| expr_has_forced_inline_call(x, funcs, caller_flatten);
    match e {
        Expr::Call { callee, args, .. } => {
            let forced = caller_flatten
                || match callee.as_ref() {
                    Expr::Ident { name, .. } => {
                        funcs.get(&name.name).is_some_and(|f| f.is_always_inline)
                    },
                    _ => false,
                };
            forced || sub(callee) || args.iter().any(sub)
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            sub(l) || sub(r)
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => sub(x),
        Expr::Cond {
            cond, then_, else_, ..
        } => sub(cond) || sub(then_) || sub(else_),
        Expr::Index { arr, idx, .. } => sub(arr) || sub(idx),
        Expr::Member { expr, .. } => sub(expr),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => inits.iter().any(|it| {
            match it {
                InitItem::Expr(x) => sub(x),
                InitItem::Designated { value, designator } => {
                    let idx_hit = match designator {
                        crate::ast::Designator::Index(x) => sub(x),
                        _ => false,
                    };
                    idx_hit || sub(value)
                },
            }
        }),
        Expr::StmtExpr { block, .. } => block.items.iter().any(|it| match it {
            BlockItem::Stmt(s) => stmt_has_forced_inline_call(s, funcs, caller_flatten),
            BlockItem::Decl(Decl::Var(v)) => v
                .init
                .as_ref()
                .is_some_and(|x| expr_has_forced_inline_call(x, funcs, caller_flatten)),
            BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().is_some_and(|b| {
                b.items.iter().any(|it| match it {
                    BlockItem::Stmt(s) => stmt_has_forced_inline_call(s, funcs, caller_flatten),
                    BlockItem::Decl(Decl::Var(v)) => v
                        .init
                        .as_ref()
                        .is_some_and(|x| expr_has_forced_inline_call(x, funcs, caller_flatten)),
                    _ => false,
                })
            }),
            _ => false,
        }),
        Expr::VaArg { ap, .. } => sub(ap),
        Expr::Offsetof { indices, .. } => indices.iter().any(|x| sub(x)),
        _ => false,
    }
}

/// [`expr_has_forced_inline_call`] 的语句版：只用来在 `StmtExpr` 体里下探。
fn stmt_has_forced_inline_call(s: &Stmt, funcs: &InlineTable, caller_flatten: bool) -> bool {
    let sub_e = |x: &Expr| expr_has_forced_inline_call(x, funcs, caller_flatten);
    let sub_s = |x: &Stmt| stmt_has_forced_inline_call(x, funcs, caller_flatten);
    match s {
        Stmt::Expr { e, .. } => sub_e(e),
        Stmt::Return { val, .. } => val.as_ref().is_some_and(sub_e),
        Stmt::Block { b, .. } => b.items.iter().any(|it| match it {
            BlockItem::Stmt(x) => sub_s(x),
            BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().is_some_and(sub_e),
            _ => false,
        }),
        Stmt::If {
            cond, then_, else_, ..
        } => sub_e(cond) || sub_s(then_) || else_.as_ref().is_some_and(|x| sub_s(x)),
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => sub_e(cond) || sub_s(body),
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().is_some_and(|x| sub_s(x))
                || cond.as_ref().is_some_and(sub_e)
                || step.as_ref().is_some_and(sub_e)
                || sub_s(body)
        },
        _ => false,
    }
}

/// 内联覆盖面补漏的归因闸门（阶段 6 批 3 补漏）。它管的两件事：
///   * 遍历下探 `Stmt::Label` / `Stmt::Switch` 臂体 / `Stmt::GotoIndirect`（受
///     [`arms_walkable`] 的「体量上限 或 线程化派发形状」约束，这是它能默认开的唯一原因）；
///   * `always_inline` 带控制流时在通用路上被接住。
///
/// **默认开**，由 15 轮同轮配对定下来（数字在方案 §15.10.8，四臂格阵）：
///
/// * 最终默认（只开体量通道，上限 800 节点）：Lua `fib(32)` **`median(r)=0.9462`**
///   （CI [0.9385,0.9639]，15/15 轮更快，基准臂 p50 292.6ms ⇒ 默认档 p50 278.3ms）；
///   同一轮 dup 臂（与基准同一二进制）自己报 0.9959 ⇒ 那 5% 远在仪器地板外；
///   SQLite `median(r)=0.9988`（CI [0.9698,1.0043]）而 dup 臂自己 0.9817 ⇒ **中性**。
/// * 再叠上「线程形 caller 不管多大都走」那一通道：Lua 反而略差（0.9529）且 `luaV_execute`
///   多 460 条 ⇒ 那条单独做成 `DCC_INLINE_WALK_THREADED`，**默认关**。
/// * 不设预算（旧记）：`-O2` 编 `sqlite3.c` 峰值 RSS 6.77~6.83GB 被 OOM 杀死（基线 5.71~6.39GB）。
///
/// 为什么要有这个闸门：它让产物**变大**（臂体展开就是把体复制进调用点：默认档上
/// sqlite3.o +459KB），而静态条数在本仓从来不能单独证明收益（§2.3 的案底：删 135 条
/// `jmp` 反而慢 3.8%）。与 `blockopt::level()`/`jumptable_enabled()` 同一条纪律：
/// `OnceLock` 只读一次 env。
fn inline_walk_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| parse_gate("DCC_INLINE_WALK", true))
}

/// 报障用的**按名字否决**名单：`DCC_INLINE_NOT=releasePage,foo` 里点名的 callee 在任何
/// 一条内联通道都不展开（等价于给它临时挂上 `noinline`）。
///
/// 为什么需要它：`-O2` 有一条成本模型驱动的通用体内联通道，验一个假设要重编一次全量；
/// 而 trace 只报 callee 不报 caller，「哪一次展开把代码弄错」从日志里看不出。有了名字
/// 名单就能按**半区**二拍：否决一批 → 重编一次 → 症状消失就说明 culprit 在这半里。
/// 与 `DCC_OPT_SKIP`/`DCC_SSA_OFF` 同一纪律：只在报障时设置，不参与任何判据，不进产物。
fn inline_not_listed(name: &str) -> bool {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("DCC_INLINE_NOT")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
    .iter()
    .any(|n| n == name)
}

/// 「臂体展开」预算的默认节点上限（caller 体的 AST 节点数）。`usize::MAX` = 不设预算。
///
/// 定值靠格阵扫描（方案 §15.10.8 的四臂格阵）：上限只影响「哪些 caller 能走进臂体」，而预算量尺本身
/// 必须是完整的 AST 节点数（见 [`stmt_node_count`] 的教训注：标签形只计 1 时预算对 Lua
/// 完全失效，那一档扫出来的「中性」不是预算的功劳）。
const INLINE_ARM_BODY_NODES_DEFAULT: usize = 800;

/// caller 体的 AST 节点数（预算的量尺）。饱和加法：预算只需要「有没有超上限」这一个比特，
/// 巨大的 TU 里让计数器顶到 `usize::MAX` 也不会变出一个「不超」的假象。
fn arm_body_nodes(body: &Block) -> usize {
    let mut nodes = 0usize;
    for it in &body.items {
        nodes = nodes.saturating_add(item_node_count(it));
    }
    nodes
}

/// 本函数的臂体展开预算（方案 §15.9-1 的第一块）：caller 体不超过 `INLINE_ARM_BODY_NODES`
/// 个 AST 节点才走进 `Label`/`Switch` 臂体。
///
/// 为什么按 caller 体量而不是按展开次数：实测 WALK 全开时 `-O2` 编 `sqlite3.c` 的峰值 RSS
/// 从 6.39GB 推到 6.77GB 并被 OOM 杀死（本机 7GB），而 `lvm.c` 只多 86MB —— 代价集中在
/// 「巨大的函数 × 几十个臂各复制一份体」那一族。先按 caller 体量挡掉那一族，小函数臂体的
/// 收益才留得下来。
///
/// 注意它**只**管「走进臂体」这一件事：顶层调用点的展开从来不受预算约束（那是批 3 之前的
/// 行为），所以不要把这一条读成「按 caller 体量限制展开」。
///
/// 上限只读一次 env（`DCC_INLINE_WALK_MAX`）：与 `blockopt::level()`/`jumptable_enabled()`
/// 同一条纪律，发射途中换规则会造出没人能复现的产物。解析失败不默默回到默认值：那会让
/// 门限扫描得到「预算不影响这一族」的假阴性，所以下一行警到 stderr（不进产物，不影响可复现）。
fn arm_budget_ok_for(body: &Block) -> bool {
    let limit = arm_budget_limit();
    if limit == usize::MAX {
        return true;
    }
    arm_body_nodes(body) <= limit
}

/// 本函数的控制流是不是**线程化派发**（threaded code）：体内有 `goto *表[..]` 站点。
///
/// 事实直接从 [`crate::ast_dispatch::collect`] 取（发射端决定「跳表基址能不能常驻
/// `%rbp`」用的是同一个判据，两边不得各自写一份）。
fn has_threaded_dispatch(body: &Block) -> bool {
    crate::ast_dispatch::collect(body)
        .tables
        .iter()
        .any(|(_, sites)| *sites > 0)
}

/// 线程形通道的开关（`DCC_INLINE_WALK_THREADED`）。**默认关**，理由是实测而非新形状看起来该赚：
/// 四臂同轮配对（方案 §15.10.8，Lua `fib(32)`，25 轮，基准=两通道全关）——
/// 只开体量通道（上限 300）`median(r)=0.9604`（CI [0.9464,0.9731]，22/25）、上限 800 **0.9704**
/// （CI [0.9437,0.9832]，22/25），而只开线程形通道 `0.9974`（不可判，与 dup 地板同带）。
/// ⇒ 那笔收益全在体量通道里，线程形那一半（`luaV_execute` 自己 +460 条）**买不到时间**，
/// 反而多付 I-cache 与编译内存 ⇒ 按 §5.5 只留机制与判据，默认关。
/// 与同族闸门同一纪律：`OnceLock` 只读一次 env。
fn threaded_channel_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| parse_gate("DCC_INLINE_WALK_THREADED", false))
}

/// 「能不能走进 `Label`/`Switch` 臂体」的总准入。
///
/// 两个通道，满一即过（各自可关）：
///   * 体量：caller ≤ `DCC_INLINE_WALK_MAX` 节点（[`arm_budget_ok_for`]）—— 小函数里臂体的
///     展开总量天然有限；
///   * 形状：函数是线程化派发（[`has_threaded_dispatch`]）—— 不管多大都走
///     （[`threaded_channel_enabled`]）。
///
/// 为什么要两个通道而不是只按体量（本轮实测）：Lua 的那笔收益**恰好住在最大的那个 caller
/// 附近** —— `luaV_execute` 是 18713 个节点，而 SQLite 的 `sqlite3VdbeExec` 是 16847 个，
/// 两者只差 10%，**没有任何一个体量阈值能既放进前者又拦下后者**（上一轮扫出的 MAX=800 能
/// 分开它们，靠的是 `stmt_node_count` 把 `Stmt::Label` 只计成 1 这个 bug，不是预算本身）。
/// 形状能分开它们（`luaV_execute` 有 80 个 `goto *disptab[..]` 站点，SQLite 在内联时点上
/// 一个也没有：它的稠密 `switch` 要到 `run_ssa_passes` 里的 `lower_switches_in_func` 才
/// 改写成线程形，而 `function_inline` 跑在它前面），但**分开它们不划算**：四臂配实测到
/// 只开线程通道对运行时没有可证效应（[`threaded_channel_enabled`] 里那四个数），所以它
/// 默认关，只留机制与本文件的判据。
///
/// 机制上为什么该这样分：线程化派发里每个 handler 尾部都是一条**间接跳转边**，把调用点展开
/// 在 handler 体内能把「跳完就碰到的那个 call」直接消掉；普通 `switch` 的 case 体之间是
/// 汇聚到同一个 join 的直跳，展开只是把体复制进去换 I-cache。
///
/// 适用边界（不得读成「线程形 unlimited」）：本版只在两个载体上量过 —— `lvm.c`（开线程通道
/// 后峰值 RSS +86MB，产物 +460 条）与 `sqlite3.c`。一个巨大的线程形 TU 仍需要重新量内存，
/// 那条列在方案 §15.9。
fn arms_walkable(body: &Block) -> bool {
    arm_budget_ok_for(body) || (threaded_channel_enabled() && has_threaded_dispatch(body))
}

/// 预算上限（env `DCC_INLINE_WALK_MAX`，只读一次）。拆成单独一个函数是为了让
/// `DCC_INLINE_TRACE` 能打出「哪个 caller 被哪条线挡住」（函数名在调用点才有）。
fn arm_budget_limit() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| match std::env::var("DCC_INLINE_WALK_MAX") {
        Ok(v) => match v.trim().parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!(
                    "dcc: 警告: DCC_INLINE_WALK_MAX={v:?} 不是非负整数，已改用默认值 {}",
                    INLINE_ARM_BODY_NODES_DEFAULT
                );
                INLINE_ARM_BODY_NODES_DEFAULT
            },
        },
        Err(_) => INLINE_ARM_BODY_NODES_DEFAULT,
    })
}

/// 「这次投机内联被成本模型否掉」的两个互不相同的理由。
///
/// 分开表达是为了让 `DCC_INLINE_TRACE` 的日志说人话：一个量的是**代码体积**
/// （节点数），另一个量的是**栈帧**（字节）。两者在 gcc 里也是两个独立的闸门
/// （`max-inline-insns-*` 与 `--param large-stack-frame`）。
enum InlineCostReject {
    /// 体量超阈值（`estimate_inline_cost` vs `STATIC_INLINE_*_THRESHOLD`）
    Cost {
        cost: usize,
        limit: usize,
        sites: usize,
    },
    /// 大局部对象闸门（[`INLINE_LARGE_LOCAL_BYTES`]）
    LargeFrame { bytes: usize, sites: usize },
}

/// 成本判据的唯一一份实现（语句级通道在用；表达式级通道放宽时要接的也是它）：
/// 多调用点的函数按 `STATIC_INLINE_COST_THRESHOLD` 卡住；只有一个调用点的放宽
/// （不复制代码）；`always_inline` 另走一档——那条上限保护的是 I-cache，它否决的是
/// 「编译器猜值不值得一展」，而这里否决的是「把一个 500 行的体复制 50 遍」那种病态
/// 标注，所以上限给到多调用点阈值的 20 倍，语料里真在用的助手成本都在几十到几百，
/// 量不到这条线。
fn stmt_splice_cost_ok(
    funcs: &InlineTable,
    callee: &str,
    f: &FuncDecl,
    body: &Block,
    args: &[Expr],
) -> Result<usize, InlineCostReject> {
    let cost = estimate_inline_cost(f, body, args);
    let sites = funcs.call_sites(callee);
    // 栈帧闸门。`__always_inline` 不在此列：那是语义要求（gcc 展开不了直接报错），
    // 不是「编译器猜值不值得一展」。
    if !f.is_always_inline {
        let bytes = max_local_object_bytes(&body.items);
        if bytes > INLINE_LARGE_LOCAL_BYTES {
            return Err(InlineCostReject::LargeFrame { bytes, sites });
        }
    }
    let limit = if sites <= 1 {
        STATIC_INLINE_SINGLE_SITE_COST_THRESHOLD
    } else if f.is_always_inline {
        STATIC_INLINE_ALWAYS_COST_THRESHOLD
    } else {
        STATIC_INLINE_COST_THRESHOLD
    };
    if cost > limit {
        return Err(InlineCostReject::Cost {
            cost,
            limit,
            sites,
        });
    }
    Ok(cost)
}

/// 把嵌在表达式里的 `__always_inline`/`flatten` 调用在**原本求值位置**展开成语句表达式。
///
/// 为什么需要它（而不是「多一个优化」）：内核 `arch/x86/include/asm/cpufeature.h` 的
/// `static_cpu_has(bit)` 展开后是
/// `(__builtin_constant_p(...) && REQUIRED_MASK_BIT_SET(...)) ? 1 : _static_cpu_has(bit)`，
/// 三元条件折成常量之前（以及 `if (cpu_feature_enabled(X) && other)` 这种 `&&` 里），
/// 调用点都不在 `call_site_shape` 已接的四形（表达式语句/赋值/尾位置/整条条件）里。
/// 不展开就不是「少内联」而是**产物错**：`_static_cpu_has` 自己那份副本的
/// `[feature] "i" (bit)` 只能拿到寄存器，`.altinstr_aux`/`.altinstructions` 里就留下
/// 一条指向寄存器名的野重定位（以前 objtool 报的 `special: can't find orig instruction`
/// 与 `vmlinux.o` 里 98 条 `→ rax` 就是它）。
///
/// 不另开一份展开机器：把这条调用暂时摆成一个临时的 `rv = callee(实参);` 语句交给
/// `try_static_inline_general`（形参绑定、标签改名、`return` 改写、出口标签那一整套
/// 都在它里），然后把它生成的块搬进 `({ ...; rv; })`。于是展开发生在调用原本的
/// 求值位置（而不是提到语句开头），短路/三元里「不该跑的那一支」不会被跑。
///
/// 档位：只接「程序员明说必须内联」的 callee（`always_inline`/`flatten`）；普通
/// `static inline` 的放宽已在 2026-11-02 试过并退回（证据与待修形状写在函数体里）。
fn inline_forced_call_as_stmt_expr(
    callee: &str,
    args: &[Expr],
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
    call_ty: Type,
    span: Span,
    allow_effectful_args: bool,
) -> Option<Expr> {
    // 与语句位置那条臂同口径：-O2 起才走通用展开机器。
    // 深度用强制内联自己的线（本函数只接强制内联的 callee，见下面的
    // `is_always_inline || caller_flatten` 判据）：内核准入链本身就有 7 层，
    // 共用 8 层会把最后一层留在外面（外带副本 ⇒ 陷阱引用）。
    let f = match funcs.get(callee) {
        Some(f) => f,
        None => return None,
    };
    let forced = f.is_always_inline || caller_flatten;
    // 档位闸门只拦「投机展开」：`-O2` 起才有成本模型参与。`__always_inline` 是语义
    // 要求（GCC 展开不了直接报错），任何档位都得展开。第二轮强制内联
    // （`function_inline(module, 0)`）正是靠这一条把上一轮内联**新生成**的调用点收干净
    // ——否则 `field_mask` 展进 caller 后露出的 `field_multiplier(field)` 没人管，
    // 产物里多一份外带副本，副本体内的 `__bad_mask()` 成为链接期未定义引用。
    if depth >= INLINE_MAX_DEPTH_FORCED || (level < 2 && !forced) {
        trace_inline_decision(
            callee,
            "REJECT",
            &format!("expr-inline: 档位或深度闸门 (level={level} depth={depth})"),
        );
        return None;
    }
    let body = match f.body.as_ref() {
        Some(b) => b,
        None => {
            trace_inline_decision(callee, "REJECT", "expr-inline: 无函数体");
            return None;
        },
    };
    // 单 `return 表达式;` 的体已由 `try_inline_call` 逐表达式替换做过（更贴源码，
    // 也不多开一个块），这里不抢。
    //
    // 但那个「让位」不能无条件：`try_inline_call` 自己还会因为**返回表达式里带标签/
    // 跳转**（`expr_jumps_out`）而拒——第二轮内联看到的就是这种体：`field_multiplier`
    // 已在第一轮把 `if (…) __bad_mask();` 展成了带出口标号的语句表达式，于是
    // `u32_encode_bits` 的体仍是单 `return`，但那个 return 里已经搬不动。此时再让位
    // 就是两边都不接，调用点留在产物里，`__bad_mask` 成为链接期未定义引用
    // （实测 `net/mac80211/tx.o`）。
    if let [BlockItem::Stmt(Stmt::Return { val: Some(ret), .. })] = body.items.as_slice() {
        if !expr_jumps_out(ret) {
            return None;
        }
    }
    // 【本通道只接强制内联 —— 放宽到普通 `static inline` 已经试过并且退回】
    //
    // 想放宽的理由是真的：调用点落在非语句位置（`return f(y) * 2`、`v += f(y)`、实参里）
    // 时三条语句级通道一条都接不到它，于是每处都留「一份外带副本 + 一次真调用」；
    // `.work_tmp/repro/inline_shapes.c` 四个形态上 dcc 留 6 个函数体而 gcc 只留 1 个
    // （.text 1661 B vs 427 B），放宽后降到 1275 B 且与 gcc 行为逐字节相同。
    //
    // 但真跑的回归把它打回来了：`check_opt_equivalence.sh` 从 0 OPT-DIFF 变成 **10 条**
    //（5 个夹具 × -O2/-O3），`cargo test --test integration` 同时挂 3 条 ——
    // 全在「把一条多语句体搬进表达式位置的 `({...})`」会碰到的形状上：
    //   c_stmt_expr.c(rc 0→128)、c_typeof.c(0→24)、
    //   c_va_arg_int128_loop_local_full_width.c(0→37)、opt_planned_frame_slots.c(0→8)、
    //   opt_switch_selector_effects.c(0→1)。
    // 也就是说：不是成本问题，是这台展开机器在表达式位置还接不住 `__builtin_va_arg`、
    // `typeof`、switch 选择子的求值序、以及帧内槽位的规划。那五条各自修好之前
    // 不能开这个口子 —— 开着就是把「少一个优化」换成「静默误编译」。
    if !forced {
        trace_inline_decision(callee, "REJECT", "expr-inline: 非强制内联（留给成本模型）");
        return None;
    }
    //  callee 自己的准入与那条臂一致（全部失败关闭）。
    if !f.is_static
        || f.is_noinline
        || f.is_variadic
        || f.returns_twice
        || f.is_nested
        || f.is_old_style
        || f.alias_name.is_some()
        || funcs.named_by_asm(callee)
        || f.params.len() != args.len()
    {
        trace_inline_decision(
            callee,
            "REJECT",
            "expr-inline: 准入 (非 static/noinline/变参/returns_twice/嵌套/别名/被 asm 点名/形参个数)",
        );
        return None;
    }
    if f.params
        .iter()
        .any(|p| p.name.is_none() || p.vla_dim.is_some() || type_has_vla_dim(&p.ty))
    {
        trace_inline_decision(callee, "REJECT", "expr-inline: 形参无名或带 VLA");
        return None;
    }
    // 返回值要送去表达式里，callee 返 void 本身就是非法 C。
    if matches!(f.ret_ty, Type::Void) {
        trace_inline_decision(callee, "REJECT", "expr-inline: 返 void 却用在值位置");
        return None;
    }
    // 体不可整体搬移（computed goto、`&&label`、嵌套函数、`static` 局部等）。
    if !body_is_splicable(body) {
        trace_inline_decision(callee, "REJECT", "expr-inline: 体不可整体搬移");
        return None;
    }
    // 实参有副作用（调用/赋值/自增）不展开：与 `Dest::Assign` 那条同判据。
    // 例外（`allow_effectful_args`）：调用点在 [`inline_expr`] 里被重试过一次，而这一形
    // 要拒的「实参里嵌着一个还得递归展开的可内联调用」已经由那次下探做完了。形参临时量
    // 只对实参求值一次，展开前后的求值次数逐字相同，所以这里不再拿它否决。
    if !allow_effectful_args && args.iter().any(crate::semantics::expr_has_side_effects) {
        trace_inline_decision(callee, "REJECT", "expr-inline: 实参有副作用");
        return None;
    }
    // 直接/间接递归：展开会爆栈。
    if calls_graph_reaches(funcs, callee, callee) {
        trace_inline_decision(callee, "REJECT", "expr-inline: 递归调用闭包");
        return None;
    }
    // 臂体预算： callee 体就是展开后的新 caller，上限按它自己算（同 `arm_budget_ok_for` 口径）。
    if !arm_budget_ok_for(body) {
        trace_inline_decision(callee, "REJECT", "expr-inline: 超出臂体展开预算");
        return None;
    }

    // 借道一条语句形状的调用点：`__inlexpN$rv = callee(实参);`
    let uid = STMT_INLINE_UID.fetch_add(1, Ordering::Relaxed);
    let rv_name = format!("__inlexp{}$rv", uid);
    let ret_ty = f.ret_ty.clone();
    let callee_expr = Expr::Ident {
        name: Ident {
            name: callee.to_string(),
            span: span.clone(),
        },
        span: span.clone(),
        ty: Type::Void,
    };
    let mut s = Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: Ident {
                    name: rv_name.clone(),
                    span: span.clone(),
                },
                span: span.clone(),
                ty: ret_ty.clone(),
            }),
            r: Box::new(Expr::Call {
                callee: Box::new(callee_expr),
                args: args.to_vec(),
                span: span.clone(),
                ty: ret_ty.clone(),
            }),
            span: span.clone(),
            ty: ret_ty.clone(),
        },
        span: span.clone(),
    };
    // 先声明结果临时量（与展开块同地），展开机器只往它写入。
    let mut items: Vec<BlockItem> = vec![BlockItem::Decl(Decl::Var(plain_var_decl(
        ret_ty.clone(),
        rv_name.clone(),
        None,
        &span,
    )))];
    if !try_static_inline_general(&mut s, funcs, level, depth, allow_effectful_args) {
        trace_inline_decision(callee, "REJECT", "expr-inline: 借道语句被通用臂拒了");
        return None;
    }
    let expanded = match s {
        Stmt::Block { b, .. } => b.items,
        _ => {
            trace_inline_decision(callee, "REJECT", "expr-inline: 展开结果不是块");
            return None;
        },
    };
    items.extend(expanded);
    // 展开体内可能还有嵌在表达式里的强制内联调用：同一个机制递归进去（深度 +1 兜底）。
    let walk_arms = arms_walkable(body);
    inline_calls_in_items(
        &mut items,
        funcs,
        level,
        caller_flatten,
        caller_is_hot,
        fenv_access,
        depth + 1,
        walk_arms,
    );
    // `({ ...; __rv; })`：语句表达式的值就是最后一个表达式。
    items.push(BlockItem::Stmt(Stmt::Expr {
        e: Expr::Ident {
            name: Ident {
                name: rv_name,
                span: span.clone(),
            },
            span: span.clone(),
            ty: ret_ty.clone(),
        },
        span: span.clone(),
    }));
    Some(Expr::StmtExpr {
        block: Box::new(Block {
            items,
            span: span.clone(),
        }),
        span,
        ty: call_ty,
    })
}

/// 「值被丢弃」的表达式位置上，对返回 **void** 的强制内联 callee 做展开。
///
/// 现场：`scoped_seqlock_read()` 把 `__scoped_seqlock_next(&_s, lock, ss_lock_irqsave)`
/// 摆在 `for (…; _s.state != ss_done; <这里>)` 的 step 上。step 的值没人读，但
/// [`inline_expr`] 只有「值位置」一条通道：`void` 的 callee 在那里被一律拒绝（先要
/// 声明结果临时量接住返回值）⇒ 调用点留在原地 ⇒ 必须另发一份 out-of-line 副本 ⇒
/// 副本里 `case ss_done: __scoped_seqlock_bug();` 的 canary 保住了一条对**故意不定义**
/// 的符号的重定位 ⇒ `ld vmlinux` 报 `undefined reference to '__scoped_seqlock_bug'`
/// （实测 `kernel/sched/build_policy.o` 两条：bug 与 invalid_target）。
///
/// 借道语句级通用臂：`Dest::Discard` 本来就不需要结果临时量（见 [`Dest`]），展开结果
/// 包成 `({ … })` 就能回到 step 位置 —— 语义上与一条表达式语句逐字等价（求值后丢弃），
/// 包出来的 `StmtExpr` 标 `void`。
fn inline_discarded_call_expr(
    e: &Expr,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
    walk_arms: bool,
    allow_unforced: bool,
) -> Option<Expr> {
    let Expr::Call { callee, span, .. } = e else {
        return None;
    };
    let Expr::Ident { name, .. } = callee.as_ref() else {
        return None;
    };
    if level < 2 || depth >= INLINE_MAX_DEPTH_FORCED {
        return None;
    }
    let f = funcs.get(&name.name)?;
    // 只接 void：有返回值的调用点在值位置本来就走 `inline_expr` 的两条通道，这里再开
    // 一条只会把「顺手丢掉返回值」写进产物。
    //
    // `allow_unforced`：逗号左侧那一格（`T x = (chk(...), expr);`）的调用点**不是**
    // 强制内联，但它和 `for` 的 step 一样是「值被丢弃」的位置 —— 不展开的代价不是
    // 少一个优化，是多一份 out-of-line 副本，而副本里的形参是运行时值，
    // `BUILD_BUG_ON(形参 > sizeof(...))` 那一族就折不掉。是否真的展开仍由
    // [`try_static_inline_general`] 的成本模型决定（拒绝了就原样留着）。
    if !matches!(f.ret_ty, Type::Void)
        || !(f.is_always_inline || caller_flatten || allow_unforced)
    {
        return None;
    }
    let mut s = Stmt::Expr {
        e: e.clone(),
        span: span.clone(),
    };
    if !try_static_inline_general(&mut s, funcs, level, depth, false) {
        return None;
    }
    let Stmt::Block { b, .. } = s else {
        return None;
    };
    let mut items = b.items;
    // 展开体内可能还有嵌在表达式里的强制内联调用：同一个机制递归进去（深度 +1 兜底）。
    inline_calls_in_items(
        &mut items,
        funcs,
        level,
        caller_flatten,
        caller_is_hot,
        fenv_access,
        depth + 1,
        walk_arms,
    );
    trace_inline_decision(
        &name.name,
        "INLINE",
        "discarded-position void call (block wrapped as stmt-expr)",
    );
    Some(Expr::StmtExpr {
        block: Box::new(Block {
            items,
            span: span.clone(),
        }),
        span: span.clone(),
        ty: Type::Void,
    })
}

fn inline_calls_in_stmt(
    s: &mut Stmt,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
    walk_arms: bool,
) {
    // always_inline / flatten 且函数体含局部声明: 语句级展开.
    // 成功后调用点已被替换为展开后的块, 无需再做表达式级内联.
    if try_stmt_inline_call(s, funcs, level, caller_flatten, depth, walk_arms) {
        return;
    }
    // P13 阶段5：-O2 起的小 `static` 函数体（带控制流）成本模型内联。展开后立刻
    // 递归走进新块（实参里可能还有可调用的直接调用），深度 +1 兜底。
    if try_static_inline_general(s, funcs, level, depth, false) {
        inline_calls_in_stmt(
            s,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth + 1,
            walk_arms,
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
                walk_arms,
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
                    walk_arms,
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
            walk_arms,
        ),
        // 循环条件与 `for` 的条件/步进同口径：以前这两支只往下走 body，条件里的
        // `__always_inline` 调用点一字未动。残留的调用点逼编译器另发一份 callee 副本，
        // 而带 `"i"(形参)` 的副本（`cpufeature.h` 的 `_static_cpu_has`）根本编不出来 ——
        // 实测 `arch/x86/kernel/fpu/xstate.o`、`arch/x86/events/core.o` 报 E0311。
        // `for (; f(); )` 一直是好的，因为 For 支有这条 cond；这里补的就差 `while`/`do`。
        //
        // 闸门 `DCC_INLINE_LOOP_COND`（默认**开**）留着是给配对 A/B 与回归时逐形退回用的：
        // 这一臂接进来时曾把本来能编的 `kernel/sched/build_utility.o` 顶坏（`do { … } while
        // (test_bit_acquire(…) && !ret)` 里 `__builtin_constant_p(形参)` 的三元两条分支都被
        // 展开，死分支带进 `Stmt::Label` 后再也剪不动）。当时用这一臂退回保住了全树，
        // 事后定案：真正的根因是**那个三元条件本身没被折**（见 `fold_expr` 的 `Expr::Cond`
        // 臂与 `prune_spliced_items`），不是循环条件不该下探；两边都修完之后
        // `wait_bit.o`、`ptrace.o` 在开着这一臂时实测 `rc=0`。
        Stmt::While { cond, body, .. } if inline_loop_cond_enabled() => {
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
                body,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
                walk_arms,
            );
        },
        Stmt::Do { body, cond, .. } if inline_loop_cond_enabled() => {
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
                body,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
                walk_arms,
            );
        },
        // 闸门关着时回到本轮之前的形状：只往下走 body，条件不动（与改前逐字一致）。
        Stmt::While { body, .. } | Stmt::Do { body, .. } => inline_calls_in_stmt(
            body,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
            walk_arms,
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
                // step 的值被丢弃（gcc 的语义与表达式语句同），所以 void 的强制内联
                // callee 在这一格上必须照样展开 —— 见 [`inline_discarded_call_expr`]。
                *st = match inline_discarded_call_expr(
                    st,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                    walk_arms,
                    false,
                ) {
                    Some(expanded) => expanded,
                    None => inline_expr(
                        st.clone(),
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth,
                    ),
                };
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
                    walk_arms,
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
                walk_arms,
            );
        },
        // 阶段 6 批 3 的补漏：臂体里的调用点要能进内联决策。两种臂体：
        //   * 源码级 computed goto（Lua）：`vmcase(l)` 展开就是 `L_OP_x: {…}`，臂体挂在
        //     `Stmt::Label` 下面（批 4 让这种函数能把跳表基址常驻 `%rbp`）；
        //   * 普通 `Stmt::Switch`（sqlite3VdbeExec 那 185 个 case 体）。
        // 时序上要记住：稠密 switch 改写成「界检 + `goto *表[idx]` + 一串标签」的
        // `lower_switches_in_func` 在 `run_ssa_passes` 里，而 `function_inline` 跑在它**前面**
        // ⇒ 内联时点上 VdbeExec 还是 `Stmt::Switch`（走下面那条臂），Label 臂接的是 Lua 那种
        // 源码级标签。两条以前都落进 `_ => {}`，所以除了最上面那一条分派，整个 VdbeExec
        // 没有任何调用点进过内联决策。
        Stmt::Label { stmt, .. } if inline_walk_enabled() && walk_arms => inline_calls_in_stmt(
            stmt,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
            walk_arms,
        ),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } if inline_walk_enabled() && walk_arms => {
            *expr = inline_expr(
                expr.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
            // `case` 的值必须是常量表达式，不对它做内联。prelude **与块体走同一套分派**
            // （`inline_calls_in_items`）：这里以前自己写了一份 match（只认 `Stmt` 与嵌套
            // `Decl::Func`），于是 `switch (x) { int y = f(); case 0: … }` 的初值调用点
            // 被默默跳过，而同样的形状在普通块里会被处理 —— 两份规则开始漂。
            inline_calls_in_items(
                prelude,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
                walk_arms,
            );
            for c in cases.iter_mut() {
                inline_calls_in_block(
                    &mut c.body,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                    walk_arms,
                );
            }
        },
        // `goto *expr` 的下标/基址里可能藏着调用（`goto *tab[f()]`）。只处理表达式，
        // 不在这一条上做任何结构改写。
        Stmt::GotoIndirect { expr, .. } if inline_walk_enabled() && walk_arms => {
            *expr = Box::new(inline_expr(
                *expr.clone(),
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            ));
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
        // `ArraySize::Var` 只是「维度写成一个表达式」，开表达式并不等于变长数组：
        // `unsigned char buf[PAGE_SIZE]`、`int tab[MAX_NR]` 都走这一臂，能常量折叠
        // 就是定长。把定长误判成 VLA 的代价是体被判「不可整体搬移」——宿主工具链里
        // sanitizer 注入的 `unsigned int guards[16]` 正是这一臂（见 sanitizer.rs 的
        // `guard_array_decl`），曾经把 `__always_inline` 助手的内联全灭。
        //
        // **元素类型必须递归**：`struct S { char d[n]; }; struct S A[2];` 的外层
        // 维度是常量 2，但元素里的 VLA 字段让整个对象仍是变长的（静态 size() 会把
        // `char d[n]` 退化成 8）。旧写法在 `Const` 臂直接 `=> false`，于是这种数组
        // 被当定长搬进 caller —— 内联后 `sizeof(struct S)` 被 SSA 折成「只剩位域
        // 容器」的 4，实测 -O2 的 `loop-interchange-15` 简化版输出 `sizeof=4`
        // （正确 8/12），同一文件的 `A[j][i].b` 读写偏移也跟着错。
        crate::ast::Type::Array(elem, sz) => match sz {
            ArraySize::Var(e) => !dim_is_const_foldable(e) || type_has_vla_dim(&elem.base),
            // 元素侧用 `has_vla()` 预筛：普通数组（`int tab[100]`）的元素没有
            // Var 维度时立即返回，不做深递归（内核 TU 的声明数以千计）。
            _ => elem.base.has_vla() && type_has_vla_dim(&elem.base),
        },
        crate::ast::Type::Ptr(p) => type_has_vla_dim(&p.base),
        crate::ast::Type::Struct(_, fields, _, _) | crate::ast::Type::Union(_, fields, _, _) => {
            fields.iter().any(|f| type_has_vla_dim(&f.ty))
        },
        _ => false,
    }
}

/// 数组维度能不能在编译期算出来（`const_eval::eval` 的整型口径）。
fn dim_is_const_foldable(e: &crate::ast::Expr) -> bool {
    crate::const_eval::eval(e).is_ok()
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
        // 内联汇编语句的操作数表达式必须跟着改名：`asm` 的输入/输出都是普通 C 表达式，
        // callee 形参被重命名成 `__inlN_...` 后，操作数还读旧名字就会读到 caller 的
        // 同名变量（或一个不存在的名字）。
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for e in outputs.iter_mut().chain(inputs.iter_mut()) {
                rename_idents_in_expr(e, map);
            }
        },
        // 标签语句与 switch 必须跟着改名：P13 阶段5 的函数体内联会把带控制流的
        // body 整体搬进 caller，漏一个分支就是「callee 的 `L` 读到 caller 的 `L`」
        //（实测 Lua `ldo.c` 内联后直接段错误）。以前的调用者（always_inline 那条臂）
        // 只接直线性 body，这两臂从未被走到。
        Stmt::Label { stmt, .. } => rename_idents_in_stmt(stmt, map),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
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
pub(crate) fn plain_var_decl(
    ty: crate::ast::Type,
    name: String,
    init: Option<Expr>,
    span: &Span,
) -> VarDecl {
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

/// `always_inline` 被通用路径接住时的上限（阶段 6 批 3 补三条路互斥的漏）。
///
/// 为什么用户明说了还要留一道闸：通用路径会插标签与 goto，把一个巨大的体复制到
/// 多个调用点是纯粹的 I-cache 代价。取值给到多调用点阈值的 20 倍 —— 语料里真在用的
/// `always_inline` 助手（SQLite btree 的 `allocateSpace`、Lua 的 `prepCallInfo`）成本都在
/// 几十到几百，量不到这条线；线上只可能是失控的手工标注，那种情况宁可留调用并
/// 打一行 REJECT（`DCC_INLINE_TRACE` 看得见）。
const STATIC_INLINE_ALWAYS_COST_THRESHOLD: usize = 20 * STATIC_INLINE_COST_THRESHOLD;

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
        InlineTable {
            sites,
            asm_names,
            funcs,
        }
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

    /// 只留「强制内联」函数的表：`is_always_inline` 的那些。
    ///
    /// 用途是「语义兜底遍历」：`__always_inline` 是语义要求，`walk_arms` 那类预算闸门
    /// 只该管成本模型（「顺便多展开几个小函数」），管不了语义。闸门一关就整块跳过的话，
    /// 臂体里的强制内联调用点会残留，DCE 因此保留 callee 的定义，而定义体里的
    /// `"i"(形参)` 根本编不出来（实测 `arch/x86/events/intel/core.c`）。
    /// 表里没有其它函数，成本模型那几条臂（成本、调用点数、闸门档位）自然全部落空，
    /// 所以补上这一趟不必再写一份「只许强制内联」的判据。
    fn forced_only(&self) -> InlineTable {
        let funcs: HashMap<String, FuncDecl> = self
            .funcs
            .iter()
            .filter(|(_, f)| f.is_always_inline)
            .map(|(k, f)| (k.clone(), f.clone()))
            .collect();
        InlineTable {
            funcs,
            sites: self.sites.clone(),
            asm_names: self.asm_names.clone(),
        }
    }
}

/// 抓出所有内联汇编文本里的标识符（只要 `call foo` / `jmp bar` 写得下，名字就算被引用）。
fn collect_asm_names(prog: &Program) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for d in &prog.decls {
        match d {
            Decl::Func(f) => {
                if let Some(b) = &f.body {
                    asm_names_in_block(b, &mut out);
                }
            },
            // 文件级 `asm(...)` 里的符号引用同样是「有人用」的证据（内核用这一族写
            // `.pushsection` 的跳表/注解，里面直接点名本 TU 的 static 函数）。
            // 不扫它，下面的跨函数 DCE 会把只在文件级 asm 里被引用的函数删掉。
            Decl::GlobalAsm { asm_str, .. } => push_asm_idents(asm_str, &mut out),
            _ => {},
        }
    }
    out
}

/// asm 文本里「不是符号引用」的位置有两类：助记符位（含它前面的指令前缀）与注释。
///
/// 助记符必须跳过，因为指令名与特殊寄存器函数的 **C 函数名同形**：
/// `arch/x86/include/asm/special_insns.h` 里就有 `wrmsr`/`rdpmc`/`clflush`/
/// `clflushopt`/`clwb`/`cpuid`/`inb`/`outb`/`insb`… 这一整族 `static inline`，
/// 而 asm 模板里写着同名指令。把它们当根等于「全 TU 的 static 函数都得留」，
/// 跨函数 DCE 在这类 TU 上完全失效。
///
/// 实测：`arch/x86/entry/vdso/vdso64/vclock_gettime.o` 因此留下 80 份外带副本
/// （`.text` 19 KB，gcc 同类对象只有 1.7 KB），其中 `wrmsr` 副本里那条
/// `if (tracepoint_enabled(write_msr))` 把 `do_trace_write_msr` 与
/// `__tracepoint_write_msr` 变成真外部引用 —— vDSO 是 `ld -shared`，直接死在
/// `undefined reference`。
///
/// 只跳「每行第一个标识符」还不够，`ALTERNATIVE()` 展开的模板里助记符前面还有东西：
/// * 指令前缀：`"rep insb"`、`"771:\n\tds wrmsr\n"`（`rep`/`ds` 先占掉首位）；
/// * 注释：`"1: # ALT: oldinstr\n…"`，`#` 之后整句都不是代码。
/// 这两条漏掉时，上述 80 份副本只降到 54 份，`wrmsr`/`insb`/`clflush` 仍在。
fn push_asm_idents(text: &str, out: &mut std::collections::HashSet<String>) {
    /// x86 里出现在助记符**前面**、自己不是符号的词。
    const PREFIXES: &[&str] = &[
        "rep", "repe", "repz", "repne", "repnz", "lock", "ds", "es", "ss", "cs", "fs",
        "gs", "data16", "data32", "addr16", "addr32", "rex", "rex.w", "rex64", "bnd",
        "notrack", "xacquire", "xrelease",
    ];
    for raw in text.split(['\n', ';']) {
        // `#` 之后是注释：`ALTERNATIVE()` 的 `# ALT: oldinstr` 整句都是注释，不截掉就会把
        // `oldinstr` 这类词当成符号。
        let line = match raw.find('#') {
            Some(p) => &raw[..p],
            None => raw,
        };
        let bytes = line.as_bytes();
        let mut i = 0usize;
        // 还在找这一句的助记符（或标号定义）。
        let mut need_mnemonic = true;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c.is_ascii_alphabetic() || c == '_' || c == '.' {
                // 被 `%`/`$`/`[`/数字粘住的不是独立名字：`%rax`、`$0`、`[err]`、`1b`。
                // `1b` 特别要紧 —— `_ASM_EXTABLE_*` 全是这种局部标号引用。
                let glued = i > 0 && {
                    let p = bytes[i - 1] as char;
                    p.is_ascii_alphanumeric() || p == '_' || p == '%' || p == '$' || p == '['
                };
                let start = i;
                // `.pushsection` 这类伪指令以点开头：点本身属于名字，但要把它连同后面的
                // 字母一起吃掉。
                i += 1;
                while i < bytes.len() {
                    let d = bytes[i] as char;
                    if d.is_ascii_alphanumeric() || d == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let word = &line[start..i];
                if glued {
                    continue;
                }
                // 标号定义（`774:`、`.Lfoo:`）后面还得读真助记符，不结束「找助记符」状态。
                let mut j = i;
                while j < bytes.len() && (bytes[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                let is_label = j < bytes.len() && bytes[j] == b':';
                if need_mnemonic {
                    // 标号定义与前缀都不算助记符，继续往后找；真助记符自己也不收。
                    if is_label {
                        continue;
                    }
                    let bare = word.trim_start_matches('.').to_ascii_lowercase();
                    if PREFIXES.contains(&bare.as_str()) {
                        continue;
                    }
                    need_mnemonic = false;
                    continue;
                }
                out.insert(word.trim_start_matches('.').to_string());
            } else {
                i += 1;
            }
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
        Stmt::If {
            cond, then_, else_, ..
        } => {
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
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
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
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
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
    /// `asm` 输出操作数（以及被内存约束取址的输入操作数）的左值根名。§5.39 的「形参
    /// 直绑实参名」判据用，见 [`asm_written_roots_of`]。
    asm_written_roots: Vec<String>,
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
            // 嵌套函数定义：闭包链绑在外层帧上，搬不动。
            //
            // **但「只有声明、没有体」的函数声明可以搬**：它只是引一个外部名字，与上面的
            // 裸 `extern` 变量声明同理。内核 `BUILD_BUG_ON` 的那条诊断函数正好是这个形状：
            // `extern void __compiletime_assert_N(void) __error__(…);` 在 AST 里是
            // `Decl::Func{ body: None }`（实测 `DCC_DUMP_PASS` 在 `fix_to_virt` 上读出的
            // 就是这一条），以前它把整族体判成不可搬移。
            BlockItem::Decl(Decl::Func(f)) if f.body.is_none() => {},
            BlockItem::Decl(Decl::Func(_)) => {
                acc.note_blocked();
            },
            BlockItem::Decl(Decl::Var(v)) => {
                // **块作用域的裸 `extern` 声明可以搬**：它只做一件事——把外部名字引进块
                // 作用域，没有存储、没有初值，搬进 caller 的同层块语义不变。
                //
                // 为什么必须放过它：内核 `include/linux/build_bug.h` 的
                // `__compiletime_assert()` 展开成
                //   `do { extern void __compiletime_assert_N(void) __error__(…);
                //         if (!(cond)) __compiletime_assert_N(); } while (0);`
                // 这条 `extern` 以前把整族体判成「不可搬移」⇒ expr-inline 拒绝 ⇒ dcc 为
                // `fix_to_virt()` 之类带 BUILD_BUG_ON 的 `static inline` 发一份外联副本，
                // 副本里那条**运行时**分支带着永远没有定义的 `__compiletime_assert_N`
                // ⇒ 全树 40 个 TU 的 `undefined reference`（实测 `apic_flat_64.o` 的
                // `fix_to_virt`：gcc 三条调用点全内联、连符号都不过产物）。
                // 展开后形参临时量是常量 ⇒ 那条 `if (!(cond))` 会被
                // [`fold_constant_branches`] 折掉，声明与调用一起消失。
                let plain_extern_decl = v.is_extern && v.init.is_none();
                if !plain_extern_decl
                    && (v.is_static
                        || v.is_extern
                        || v.is_thread_local
                        || v.cleanup_fn.is_some()
                        || v.register_name.is_some()
                        || v.alias_name.is_some()
                        || v.asm_label.is_some()
                        || type_has_vla_dim(&v.ty))
                {
                    note_blocked_node("var", || format!("{v:?}"), acc);
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
            _ => note_blocked_node("item", || format!("{item:?}"), acc),
        }
    }
}

fn scan_block(b: &Block, acc: &mut InlineScan) {
    scan_items(&b.items, acc);
}

/// 排障用：`DCC_INLINE_BLOCK_TRACE=1` 打出「把 callee 体判成不可搬移」的那个节点。
/// 没有它只能看到「体不可整体搬移」这一句，而那一句背后有十几个分叉（nested asm、
/// 复合字面量、语句表达式里往外跳 …），逐个猜形状会绕很久。
///
/// `dbg` 必须**惰性**：`Decl::Var` / `BlockItem` 的 `Debug` 会顺着 `QualType` 展开
/// `Arc<Vec<Field>>` / `Box<QualType>` 这层共享类型 DAG，单个内核 TU 上每次展开都是
/// 百 MB 级（`net/unix/af_unix.o` 实测爬满 6.7 GB 被 OOM 杀，`-O0` 同样复现）。
/// 这里按值收 `String` 就等于「不管开不开诊断都先做一遍展开」，而 `scan_items` 是
/// 递归下探的（一个 TU 上千帧），叠起来是平方级。
fn inline_block_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DCC_INLINE_BLOCK_TRACE").is_some())
}

fn note_blocked_node(tag: &str, dbg: impl FnOnce() -> String, acc: &mut InlineScan) {
    if inline_block_trace_on() {
        let mut d = dbg().replace('\n', " ");
        d.truncate(d.char_indices().nth(180).map(|(i, _)| i).unwrap_or(d.len()));
        eprintln!("BLOCK-{tag}| {d}");
    }
    acc.note_blocked();
}

fn scan_stmt(s: &Stmt, acc: &mut InlineScan) {
    match s {
        // computed goto：标签地址进了 callee 就与 caller 的标签集混在一起，不能搬。
        Stmt::GotoIndirect { .. } => acc.note_blocked(),
        // `asm goto` **可以**搬：它的跳转目标是函数局部的 C 标签，`prefix_labels_in_stmt`
        // 会给标签定义、`goto` 与模板里的 `%l[name]`/`%c[name]` 文本一起改名（一次展开一份
        // 前缀，与 caller 的同名标签不相撞）。原来把它和 computed goto 一并拒绝，代价是
        // 内核 `arch_static_branch`（`arch/x86/include/asm/jump_label.h`）那种「asm goto +
        // 标签 + 两个 return」的 `__always_inline` 体永远不内联：它的 `"i" (key)` 只在
        // 内联后才退化成 `&全局变量` 这种链接期常量，不内联就落成寄存器，模板里的
        // `.long %c0 + %c1 - .` 变成指向寄存器名的野重定位（`__jump_table`/`.altinstructions`
        // 条目整片坏掉，要到 objtool 才暴露）。操作数仍是普通 C 表达式，照常扫调用。
        Stmt::AsmGoto {
            outputs,
            inputs,
            input_constraints,
            ..
        } => {
            note_asm_operand_roots(outputs, inputs, input_constraints, acc);
            for e in outputs.iter().chain(inputs.iter()) {
                scan_expr(e, acc);
            }
        },
        Stmt::Expr { e, .. } => scan_expr(e, acc),
        Stmt::Return { val: Some(e), .. } => scan_expr(e, acc),
        Stmt::Return { val: None, .. }
        | Stmt::Goto { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. } => {},
        Stmt::If {
            cond, then_, else_, ..
        } => {
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
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
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
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            scan_expr(expr, acc);
            scan_items(prelude, acc);
            for c in cases {
                if let Some(v) = &c.val {
                    scan_expr(v, acc);
                }
                scan_block(&c.body, acc);
            }
        },
        _ => note_blocked_node("stmt", || format!("{s:?}"), acc),
    }
}

fn scan_expr(e: &Expr, acc: &mut InlineScan) {
    match e {
        // 语句表达式**可以**搬：它自成一个块作用域，`rename_idents_in_expr` /
        // `rename_decl_names_in_expr` 都已经下探到块里，里面的局部声明改名与引用改名是
        // 一致的；`break`/`continue` 只能绑它自己的循环（C 不允许跨语句表达式跳进），
        // 搬过去语义不变。
        //
        // 为什么这条必须开：内核 `VM_WARN_ON*`/`BUILD_BUG_ON` 展开的 `({ … })` 几乎全在
        // `__always_inline` 的体里，拒掉它等于把这批体全部拒掉 —— 实测 `mm/memory.o`
        // 因此留下 362 个 `static inline` 外联副本（gcc 是 0 个），副本里的
        // `switch (形参) { … default: BUILD_BUG(); }` 折不掉，把 `__compiletime_assert_NNN`
        // 的引用带进了产物。
        //
        // 两类「往外指」的东西仍拒，见 [`stmt_expr_jumps_out`]。
        Expr::StmtExpr { block, .. } => {
            if stmt_expr_jumps_out(&block.items) {
                acc.note_blocked();
            } else {
                scan_block(block, acc);
            }
        },
        // 这两个形态隐含「帧/标签就在本函数里」，搬进调用方就错。
        Expr::LabelAddr { .. } | Expr::VaArg { .. } | Expr::VaArgPack { .. } => {
            acc.note_blocked()
        },
        // 表达式位置的 `asm`（`x = asm(...)` 与把 asm 写成语句时 parser 给的形态）
        // **可以搬**：它与不带 goto 标签的 `Stmt::AsmGoto` 是同一个东西，操作数都是普通
        // C 表达式（改名由 `rename_idents_in_expr` 的 AsmExpr 臂负责），不引用本函数的标签。
        // 以前它与上面几类一并拒掉，代价是内核 `arch/x86/include/asm/asm.h` 的
        // `rip_rel_ptr`（体就是「`asm("leaq %c1(%%rip), %0" : "=r"(p) : "i"(p)); return p;`」）
        // 永远不内联：它的 `"i" (p)` 只有展开后才是 `&全局变量` 这种链接期常量，不展开就
        // 只能拿到寄存器，`%c1` 被换成空串 ⇒ 编出语义错且不报错的 `leaq (%rip), %rax`。
        // 仍不接 `asm goto` 标签引用：那条由 `Stmt::AsmGoto` 自己那一臂管，标签改名在那里做。
        Expr::AsmExpr {
            outputs,
            inputs,
            input_constraints,
            ..
        } => {
            note_asm_operand_roots(outputs, inputs, input_constraints, acc);
            for e in outputs.iter().chain(inputs.iter()) {
                scan_expr(e, acc);
            }
        },
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
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            scan_expr(cond, acc);
            scan_expr(then_, acc);
            scan_expr(else_, acc);
        },
        Expr::Index { arr, idx, .. } => {
            scan_expr(arr, acc);
            scan_expr(idx, acc);
        },
        Expr::Member { expr, .. } => scan_expr(expr, acc),
        // 设计化初始化器/复合字面量：**可以**搬。
        //
        // 两条配套改写器（[`rename_idents_in_expr`] 与 [`inline_rename_expr`]）都已经
        // 下探到 `inits`，且只改初始化值、不改 designator 里的字段名；块作用域的复合
        // 字面量有自动存储期，搬进 caller 的块里只是把寿命延长到 caller 的块尾（C 允许，
        // 求值点不变）。
        //
        // 以前这一族整体拒绝（「失败关闭」），代价是内核「复合字面量造值」宏
        // （`(pte_t){ .pte = val }`、`((pgprot_t){ … })`、`(struct desc_ptr){…}`）把整族
        // `static inline` 体钉成不可搬移 ⇒ 只能留外带副本 ⇒ 副本里 `switch (形参)` 的
        // `default: BUILD_BUG()` 折不掉 ⇒ 陷阱函数引用。实测 `mm/memory.o` 一次回放的
        // 拒绝名单里，`DCC_INLINE_BLOCK_TRACE` 记到 625 次是 `(pte_t){.pte=…}`、
        // 142 次是 `(pmd_t){…}`、96 次是 `(pud_t){…}`，合计一千多条。
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => scan_expr(x, acc),
                    InitItem::Designated { value, designator } => {
                        if let crate::ast::Designator::Index(idx) = designator {
                            scan_expr(idx, acc);
                        }
                        scan_expr(value, acc);
                    },
                }
            }
        },
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
        _ => note_blocked_node("expr", || format!("{e:?}"), acc),
    }
}

/// `({ … })` 里有没有「指向语句表达式外面」的东西：`return` 与标签/`goto`。
///
/// 内联的两件配套改写工具都不下探语句表达式：`rewrite_returns_to_dest`（把 callee 的
/// `return` 改成「写目的地 + goto 出口」）与 `prefix_labels_in_stmt`（给标签加 `__inlN__l`
/// 前缀避免与调用方同名）：
///  * 漏掉前者 ⇒ 语句表达式里的 `return` 会从**调用方**返回（值还写错位置）；
///  * 漏掉后者 ⇒ 同一个 body 内联两次时两份标签相撞，或与调用方同名标签相撞。
/// 这两类在语料里不是主力（内核宏体里几乎只有声明＋`if`＋表达式），拒掉不损失覆盖面。
fn stmt_expr_jumps_out(items: &[BlockItem]) -> bool {
    items.iter().any(|it| match it {
        BlockItem::Stmt(s) => stmt_jumps_out(s),
        BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().map(expr_jumps_out).unwrap_or(false),
        BlockItem::Decl(Decl::Func(f)) => f
            .body
            .as_ref()
            .map(|b| stmt_expr_jumps_out(&b.items))
            .unwrap_or(false),
        _ => false,
    })
}

fn stmt_jumps_out(s: &Stmt) -> bool {
    match s {
        Stmt::Return { .. }
        | Stmt::Label { .. }
        | Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. } => true,
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_jumps_out(cond)
                || stmt_jumps_out(then_)
                || else_.as_ref().map(|e| stmt_jumps_out(e)).unwrap_or(false)
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            expr_jumps_out(cond) || stmt_jumps_out(body)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().map(|i| stmt_jumps_out(i)).unwrap_or(false)
                || cond.as_ref().map(expr_jumps_out).unwrap_or(false)
                || step.as_ref().map(expr_jumps_out).unwrap_or(false)
                || stmt_jumps_out(body)
        },
        Stmt::Block { b, .. } => stmt_expr_jumps_out(&b.items),
        Stmt::Switch {
            expr,
            prelude,
            cases,
            ..
        } => {
            expr_jumps_out(expr)
                || stmt_expr_jumps_out(prelude)
                || cases.iter().any(|c| stmt_expr_jumps_out(&c.body.items))
        },
        _ => false,
    }
}

/// 表达式子树里有没有语句表达式，且那颗语句表达式往外指。
fn expr_jumps_out(e: &Expr) -> bool {
    let mut hit = false;
    visit_expr_stmtexprs(e, &mut |items| {
        if stmt_expr_jumps_out(items) {
            hit = true;
        }
    });
    hit
}

/// 遍历表达式里所有语句表达式的项列表（只读版，内联准入用）。
///
/// `pub(crate)` 是给 `ipa_modref::proven_const_locals` 用的：那份候选表必须把
/// `({ int ux = …; … })` 里的声明一并收进来，否则 `min()/max()` 的
/// `__careful_cmp_once` 里那两个 `auto` 局部永远进不了表（见该处注释）。
pub(crate) fn visit_expr_stmtexprs<'a>(e: &'a Expr, f: &mut dyn FnMut(&'a [BlockItem])) {
    match e {
        Expr::StmtExpr { block, .. } => f(&block.items),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            visit_expr_stmtexprs(l, f);
            visit_expr_stmtexprs(r, f);
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
        | Expr::AlignofExpr { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::VaArg { ap: x, .. } => visit_expr_stmtexprs(x, f),
        Expr::Index { arr, idx, .. } => {
            visit_expr_stmtexprs(arr, f);
            visit_expr_stmtexprs(idx, f);
        },
        Expr::Call { callee, args, .. } => {
            visit_expr_stmtexprs(callee, f);
            for a in args {
                visit_expr_stmtexprs(a, f);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            visit_expr_stmtexprs(cond, f);
            visit_expr_stmtexprs(then_, f);
            visit_expr_stmtexprs(else_, f);
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            for o in inputs.iter().chain(outputs.iter()) {
                visit_expr_stmtexprs(o, f);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => visit_expr_stmtexprs(x, f),
                    InitItem::Designated { value, .. } => visit_expr_stmtexprs(value, f),
                }
            }
        },
        _ => {},
    }
}

/// 收集一个函数体里出现的**直接调用**名字（用于递归/环判定）。
fn collect_direct_calls(body: &Block) -> Vec<String> {
    let mut acc = InlineScan::default();
    scan_block(body, &mut acc);
    acc.calls
}

/// `funcs[from]` 的调用闭包能否回到 `target`（间接递归也算）。
fn calls_graph_reaches(funcs: &InlineTable, from: &str, target: &str) -> bool {
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
///   * `goto *ptr` / `Expr::LabelAddr`：标签地址进了 callee 就
///     与 caller 的标签集混在一起，而且 computed goto 本身也不允许搬
///     （`Stmt::AsmGoto` 不在此列：它的标签是函数局部的，改名后可搬）；
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
        if INLINE_SPLICE_BLOCKING_BUILTINS.contains(&n) || KNOWN_RETURNS_TWICE.contains(&n) {
            if inline_block_trace_on() {
                eprintln!("BLOCK-call| {n}");
            }
            return false;
        }
    }
    true
}

/// 搬移后**换了主人**的内建：它们读的是「本函数」的帧/入参状态，整体搬进调用方就指向
/// 了别的帧。
///
/// 以前这里是 `name.starts_with("__builtin")`，一条土法全部拒。代价在真实语料上很直接：
/// 内核每一个带 `likely()/unlikely()`（`__builtin_expect`）的 `static inline` 体都因此变成
/// “不可整体搬移” ⇒ 只能留外带副本（实测 `mm/rmap.o` 的 `__folio_add_rmap`：体里只有
/// 一个 `unlikely()`，就被钉成外带副本，副本里 `switch (level)` 的 `default: BUILD_BUG()`
/// 折不掉，最终变成 `__compiletime_assert_*` 的未定义引用）。
///
/// 留在这张表里的都是硬事实，不是估计：变参保存区（`__builtin_va_*`/`__builtin_next_arg`）、
/// 返回地址与帧地址（`__builtin_return_address`/`frame_address`/`dwarf_cfa`）、二次返回
/// （`setjmp`/`longjmp`）、`__builtin_apply*`。其余内建在展开后语义不变甚至更好：
/// `__builtin_expect` 只是个提示，`__builtin_constant_p`/`__builtin_object_size` 正是
/// “内联之后才能算出来”的那一族，扔掉它们等于抹掉 gcc 的整个常量折叠收益。
const INLINE_SPLICE_BLOCKING_BUILTINS: &[&str] = &[
    "__builtin_va_start",
    "__builtin_va_end",
    "__builtin_va_copy",
    "__builtin_va_arg",
    "__builtin_next_arg",
    "__builtin_return_address",
    "__builtin_frame_address",
    "__builtin_extract_return_addr",
    "__builtin_dwarf_cfa",
    "__builtin_setjmp",
    "__builtin_longjmp",
    "__builtin_apply",
    "__builtin_apply_args",
    "__builtin_eh_return",
    "__builtin_return",
];

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
        // `asm goto` 的标签引用有两个身份：AST 里的 `goto_labels`（codegen 据此解析
        // `%l[...]`）和模板字符串里的字面 `%l[name]`/`%c[name]`。只改前者会让 codegen 的
        // 替换按新名去找旧名文本，模板里剩下没人认领的 `%l[l_yes]` —— 展开出来的
        // 汇编直接引用一个不存在的标签。文本匹配带完整 `]` 收尾，不会误伤同名前缀
        // （`l_yes` 与 `l_yes_all` 各自只匹配自己那一种写法）。
        Stmt::AsmGoto {
            asm_str,
            goto_labels,
            ..
        } => {
            for label in goto_labels.iter_mut() {
                let old = label.name.clone();
                label.name = format!("{}{}", prefix, old);
                *asm_str = asm_str
                    .replace(&format!("%l[{}]", old), &format!("%l[{}]", label.name))
                    .replace(&format!("%c[{}]", old), &format!("%c[{}]", label.name));
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

// ---------------------------------------------------------------------------
// asm「只能接立即数」操作数的常量代回（内联形参临时量与写一次的常量局部同一机制）
// ---------------------------------------------------------------------------

/// 一条 `VarDecl` 的初值能不能作为代回值。
///
/// 判据是「这个初值是编译期常量」（`const_eval::eval_full` 能算出来），而不是
/// 「它无副作用」：无副作用的表达式仍可能读到一个会变的量（`int t = g;` 里的 `g` 在
/// 两次读之间可能被人改），代过去就不是原来的值。常量初值不依赖任何可变状态，
/// 搬到哪里都是同一个值。带特殊存储期/属性的声明不接（`static` 局部可被别的调用改）。
fn const_foldable_init(v: &VarDecl) -> Option<Expr> {
    if v.is_static
        || v.is_extern
        || v.is_thread_local
        || v.cleanup_fn.is_some()
        || v.register_name.is_some()
        || v.asm_label.is_some()
        // **`volatile` 局部永远不是候选**：每一次读都是一次可观察的副作用（C11 6.7.3p6），
        // 把它代回成常量等于把那次访问删掉。以前这条不在排除名单里，只在「asm 立即数
        // 操作数」那一小范围内危害有限（那里的语义本来就是「非要编译期常量」）；分支条件
        // 一旦也做普通读代回，`volatile int t=1; if(!t)` 就会被错折成不可达。
        || crate::ast::type_has_volatile(&v.ty)
    {
        return None;
    }
    let e = v.init.as_ref()?;
    match const_eval::eval_full(e, false) {
        Ok(crate::const_eval::ConstValue::Int(_))
        | Ok(crate::const_eval::ConstValue::Addr { .. })
        | Ok(crate::const_eval::ConstValue::Str { .. })
        | Ok(crate::const_eval::ConstValue::Bytes { .. }) => Some(e.clone()),
        _ => None,
    }
}

/// 表达式 `e` 的**根**是否就是一个名字（`p = ...`、`++p`、`&p` 这类整体写/取址）。
///
/// 只认根：`p->x = ...`、`p[0] = ...`、`*p = ...` 改的是 `p` 指向的内存，`p` 这个
/// 指针值本身没变，读 `p` 仍然可以代回实参。
fn ident_root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(name.name.clone()),
        _ => None,
    }
}

/// 收集一棵表达式子树里被写入或被取地址的候选名（写入 = 值不再等于实参）。
///
/// 与 [`rename_idents_in_expr`] 走同一批变体臂：漏一条只是少一次代回（失败关闭），
/// 不会把已经脏了的候选代回去。
fn collect_cand_writes_expr(
    e: &Expr,
    cands: &HashMap<String, Expr>,
    out: &mut std::collections::HashSet<String>,
) {
    let mark = |x: &Expr, out: &mut std::collections::HashSet<String>| {
        if let Some(n) = ident_root_name(x) {
            if cands.contains_key(&n) {
                out.insert(n);
            }
        }
    };
    match e {
        Expr::Assign { l, r, .. } => {
            mark(l, out);
            collect_cand_writes_expr(l, cands, out);
            collect_cand_writes_expr(r, cands, out);
        },
        Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. } => {
            mark(x, out);
            collect_cand_writes_expr(x, cands, out);
        },
        // `&p`：地址逃出去了，被调方可以透过它改 `p` 本身 ⇒ 这个候选之后不可代回。
        // `&p->x` / `&p[i]` 的根不是 `p`，只是从 `p` 算出的地址，不动 `p` 的值。
        Expr::AddrOf { e: x, .. } => {
            mark(x, out);
            collect_cand_writes_expr(x, cands, out);
        },
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_cand_writes_expr(l, cands, out);
            collect_cand_writes_expr(r, cands, out);
        },
        Expr::Call { callee, args, .. } => {
            collect_cand_writes_expr(callee, cands, out);
            for a in args {
                collect_cand_writes_expr(a, cands, out);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_cand_writes_expr(cond, cands, out);
            collect_cand_writes_expr(then_, cands, out);
            collect_cand_writes_expr(else_, cands, out);
        },
        Expr::Index { arr, idx, .. } => {
            collect_cand_writes_expr(arr, cands, out);
            collect_cand_writes_expr(idx, cands, out);
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => collect_cand_writes_expr(x, cands, out),
        Expr::Member { expr, .. } => collect_cand_writes_expr(expr, cands, out),
        Expr::VaArg { ap, .. } => collect_cand_writes_expr(ap, cands, out),
        Expr::Offsetof { indices, .. } => {
            for i in indices {
                collect_cand_writes_expr(i, cands, out);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => collect_cand_writes_expr(x, cands, out),
                    InitItem::Designated { value, designator } => {
                        if let crate::ast::Designator::Index(idx) = designator {
                            collect_cand_writes_expr(idx, cands, out);
                        }
                        collect_cand_writes_expr(value, cands, out);
                    },
                }
            }
        },
        Expr::AsmExpr {
            outputs,
            output_constraints,
            inputs,
            ..
        } => {
            // 表达式位置的 asm 同样要回写输出操作数（与语句版同口径）。
            for (o, c) in outputs.iter().zip(output_constraints.iter()) {
                if !c.is_empty() {
                    mark(o, out);
                }
                collect_cand_writes_expr(o, cands, out);
            }
            for i in inputs {
                collect_cand_writes_expr(i, cands, out);
            }
        },
        Expr::StmtExpr { block, .. } => collect_cand_writes_items(&block.items, cands, out),
        // 叶子：字面量与名字读。
        _ => {},
    }
}

fn collect_cand_writes_items(
    items: &[BlockItem],
    cands: &HashMap<String, Expr>,
    out: &mut std::collections::HashSet<String>,
) {
    for it in items {
        match it {
            BlockItem::Stmt(s) => collect_cand_writes_stmt(s, cands, out),
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &v.init {
                    collect_cand_writes_expr(e, cands, out);
                }
            },
            _ => {},
        }
    }
}

fn collect_cand_writes_stmt(
    s: &Stmt,
    cands: &HashMap<String, Expr>,
    out: &mut std::collections::HashSet<String>,
) {
    match s {
        Stmt::AsmGoto {
            outputs,
            inputs,
            output_constraints,
            ..
        } => {
            // asm 的输出操作数一律是写（`=`/`+`/匹配约束都要回写）。
            for (o, c) in outputs.iter().zip(output_constraints.iter()) {
                if !c.is_empty() {
                    if let Some(n) = ident_root_name(o) {
                        if cands.contains_key(&n) {
                            out.insert(n);
                        }
                    }
                }
                collect_cand_writes_expr(o, cands, out);
            }
            for i in inputs {
                collect_cand_writes_expr(i, cands, out);
            }
        },
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            collect_cand_writes_expr(e, cands, out)
        },
        Stmt::Return { val: None, .. }
        | Stmt::Goto { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => {},
        Stmt::If { then_, else_, .. } => {
            collect_cand_writes_stmt(then_, cands, out);
            if let Some(e) = else_ {
                collect_cand_writes_stmt(e, cands, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            collect_cand_writes_expr(cond, cands, out);
            collect_cand_writes_stmt(body, cands, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_cand_writes_stmt(i, cands, out);
            }
            if let Some(c) = cond {
                collect_cand_writes_expr(c, cands, out);
            }
            if let Some(st) = step {
                collect_cand_writes_expr(st, cands, out);
            }
            collect_cand_writes_stmt(body, cands, out);
        },
        Stmt::Block { b, .. } => collect_cand_writes_items(&b.items, cands, out),
        Stmt::Label { stmt, .. } => collect_cand_writes_stmt(stmt, cands, out),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            collect_cand_writes_expr(expr, cands, out);
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    collect_cand_writes_stmt(st, cands, out);
                }
            }
            for c in cases {
                collect_cand_writes_items(&c.body.items, cands, out);
            }
        },
        Stmt::Invoke { call, .. } => collect_cand_writes_stmt(call, cands, out),
        Stmt::GotoIndirect { .. } => {},
        _ => {},
    }
}

/// 把一棵表达式子树里「读候选临时量」的 `Ident` 换成 caller 的实参。
///
/// 只动还没被标脏的候选；实参属 caller 作用域，替换后不再参与 callee 改名。
fn subst_cand_reads_expr(
    e: &mut Expr,
    cands: &HashMap<String, Expr>,
    dirty: &std::collections::HashSet<String>,
) {
    match e {
        Expr::Ident { name, .. } => {
            if let (Some(arg), false) = (cands.get(&name.name), dirty.contains(&name.name)) {
                *e = arg.clone();
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            subst_cand_reads_expr(l, cands, dirty);
            subst_cand_reads_expr(r, cands, dirty);
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
        | Expr::AlignofExpr { e: x, .. } => subst_cand_reads_expr(x, cands, dirty),
        Expr::Call { callee, args, .. } => {
            subst_cand_reads_expr(callee, cands, dirty);
            // 同 [`subst_cprop_expr_scoped`]：`__builtin_constant_p` 的操作数不求值。
            if !is_syntactic_query_callee(callee) {
                for a in args {
                    subst_cand_reads_expr(a, cands, dirty);
                }
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            subst_cand_reads_expr(cond, cands, dirty);
            subst_cand_reads_expr(then_, cands, dirty);
            subst_cand_reads_expr(else_, cands, dirty);
        },
        Expr::Index { arr, idx, .. } => {
            subst_cand_reads_expr(arr, cands, dirty);
            subst_cand_reads_expr(idx, cands, dirty);
        },
        Expr::Member { expr, .. } => subst_cand_reads_expr(expr, cands, dirty),
        // 注意：语句表达式的**块**需要「收集 + 代回」两步，而本函数的候选表是只读的
        // （调用方里有一条作用域版本传 `&`）。所以下探放在有 `&mut` 的那一层：
        // [`subst_cand_reads_stmt`] 的表达式语句臂与 [`subst_cand_reads_items`] 的声明初值。
        _ => {},
    }
}

/// 走一棵 **C 表达式**，只在 `Expr::AsmExpr`（表达式位置的内联汇编）的
/// 「只能接立即数」输入操作数上做代回。
///
/// 普通 C 读**不代回**：那是等值改写，但会把产物从「读临时量」变成「读实参」，
/// 与本轮之前的产物无谓漂移；只有非要常量不可的操作数才有必要。
fn subst_asm_immediate_ops_in_expr(
    e: &mut Expr,
    cands: &mut HashMap<String, Expr>,
    dirty: &mut std::collections::HashSet<String>,
) {
    match e {
        Expr::AsmExpr {
            inputs,
            input_constraints,
            outputs,
            ..
        } => {
            for (op, c) in inputs.iter_mut().zip(input_constraints.iter()) {
                if crate::codegen::shared::asm_constraints::constraint_is_immediate_only(c) {
                    subst_cand_reads_expr(op, cands, dirty);
                }
            }
            // 操作数子树里可能嵌着普通表达式（甚另一条 asm），继续下探
            for op in inputs.iter_mut().chain(outputs.iter_mut()) {
                subst_asm_immediate_ops_in_expr(op, cands, dirty);
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            subst_asm_immediate_ops_in_expr(l, cands, dirty);
            subst_asm_immediate_ops_in_expr(r, cands, dirty);
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. } => subst_asm_immediate_ops_in_expr(x, cands, dirty),
        Expr::Call { callee, args, .. } => {
            subst_asm_immediate_ops_in_expr(callee, cands, dirty);
            for a in args {
                subst_asm_immediate_ops_in_expr(a, cands, dirty);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            subst_asm_immediate_ops_in_expr(cond, cands, dirty);
            subst_asm_immediate_ops_in_expr(then_, cands, dirty);
            subst_asm_immediate_ops_in_expr(else_, cands, dirty);
        },
        Expr::Index { arr, idx, .. } => {
            subst_asm_immediate_ops_in_expr(arr, cands, dirty);
            subst_asm_immediate_ops_in_expr(idx, cands, dirty);
        },
        Expr::Member { expr, .. } => subst_asm_immediate_ops_in_expr(expr, cands, dirty),
        // 语句表达式：内核 `__WARN_bug_entry(...)` 就是 `({ struct bug_entry *bug; asm ...; bug; })`，
        // 那条 asm 与它的常量局部（`int __flags = ...`）都在这个块里；不进来就根本不会
        // 访问到这条 asm，`[fl] "i" (__flags)` 于是始终落在寄存器上。按项走一遍还能让
        // 块内的声明正常压栈/弹栈（作用域与外层一致）。
        Expr::StmtExpr { block, .. } => subst_cand_reads_items(&mut block.items, cands, dirty),
        // 初始化器里的 asm（内联展开后的 `({ ... })` 落在 `{ .f = ... }` 里，内核
        // `arch/x86/boot/startup/gdt_idt.c` 的 `struct desc_ptr d = { .address = ... }`
        // 就是这个形状）：不下探就永远看不到那条 asm，形参临时量也就没有代回。
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits.iter_mut() {
                match it {
                    InitItem::Expr(x) => subst_asm_immediate_ops_in_expr(x, cands, dirty),
                    InitItem::Designated { value, .. } => {
                        subst_asm_immediate_ops_in_expr(value, cands, dirty)
                    },
                }
            }
        },
        Expr::VaArg { ap, .. } => subst_asm_immediate_ops_in_expr(ap, cands, dirty),
        Expr::Offsetof { indices, .. } => {
            for idx in indices {
                subst_asm_immediate_ops_in_expr(idx, cands, dirty);
            }
        },
        _ => {},
    }
}

/// 按语句顺序走一遍搬进来的 callee 体：在「非要常量不可」的 asm 输入操作数上代回
/// 实参，并把本条语句的写标脏。
///
/// asm 输出的写只影响**之后**的语句，所以同一条语句里「`"=r"(p)` 写 + `"i"(p)` 读」
/// 的形状（内核 `rip_rel_ptr`）仍能代回：输入操作数在 asm 执行前求值，此刻 `p` 还没
/// 被写过。输出操作数**不代回**：`"=r"(p)` 要写的是 callee 的形参（按值传递，写完就丢），
/// 代回实参会把值写进 caller 的变量，语义直接变。
fn subst_cand_reads_stmt(
    s: &mut Stmt,
    cands: &mut HashMap<String, Expr>,
    dirty: &mut std::collections::HashSet<String>,
) {
    match s {
        Stmt::AsmGoto {
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            ..
        } => {
            for (op, c) in inputs.iter_mut().zip(input_constraints.iter()) {
                if crate::codegen::shared::asm_constraints::constraint_is_immediate_only(c) {
                    subst_cand_reads_expr(op, cands, dirty);
                }
            }
            // 本条语句的写：asm 输出操作数（非空约束即回写）+ 操作数子树里的 C 写。
            let mut w = std::collections::HashSet::new();
            for (o, c) in outputs.iter().zip(output_constraints.iter()) {
                if !c.is_empty() {
                    if let Some(n) = ident_root_name(o) {
                        if cands.contains_key(&n) {
                            w.insert(n);
                        }
                    }
                }
                collect_cand_writes_expr(o, cands, &mut w);
            }
            for i in inputs.iter() {
                collect_cand_writes_expr(i, cands, &mut w);
            }
            dirty.extend(w);
        },
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            // 【语句表达式作为整条语句】强制内联把 callee 体拼成 `({ … })` 后，形参
            // 实体化出来的临时量（`unsigned long __inlN$_usec = 50;`）就坐在这个**块**里。
            // 不进这一层，块里的 `if (usec >= DELAY_CONST_MAX)` 选择子永远是 Ident ⇒ 折不成
            // 常量 ⇒ 死支里那个「只有声明、没有定义」的陷阱调用留在产物里（实测
            // `drivers/usb/host/pci-quirks.o` 的 `__bad_udelay`，以及 bitfield 一族的
            // `__field_overflow`/`__bad_mask`）。
            // 块的候选用同一个收集器跑：`subst_cand_reads_items` 自己会 push/pop 作用域，
            // 块内新候选不溢出到块外，块外的候选在块内仍然可见（C 的作用域就是这样的）。
            if let Expr::StmtExpr { block, .. } = e {
                subst_cand_reads_items(&mut block.items, cands, dirty);
            }
            subst_asm_immediate_ops_in_expr(e, cands, dirty);
            let mut w = std::collections::HashSet::new();
            collect_cand_writes_expr(e, cands, &mut w);
            dirty.extend(w);
            // 常量重赋值：`v = <常量>` 之后 v 的「当前值」是 RHS，而不是「从此作废」。
            // 收集器不区分 RHS 是不是常量，所以在这里把这一种写重新抬回来：
            // 内核 `__flags` 一类写法有「先声明再赋常量」的形状，只作废会让这些点
            // 重新落回寄存器（GCC 会跟着传播那个常量）。
            if let Expr::Assign {
                op: BinOp::Add,
                l,
                r,
                ..
            } = &*e
            {
                if let Some(n) = ident_root_name(l) {
                    if cands.contains_key(&n) && const_eval::eval_full(r, false).is_ok() {
                        dirty.remove(&n);
                        cands.insert(n, (**r).clone());
                    }
                }
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            let mut w = std::collections::HashSet::new();
            collect_cand_writes_expr(cond, cands, &mut w);
            dirty.extend(w);
            subst_asm_immediate_ops_in_expr(cond, cands, dirty);
            // 两支各拿一份状态，汇合点取并集：任一支写过该候选，之后都不能再代回。
            let mut d_then = dirty.clone();
            subst_cand_reads_stmt(then_, cands, &mut d_then);
            let mut d_else = dirty.clone();
            if let Some(el) = else_ {
                subst_cand_reads_stmt(el, cands, &mut d_else);
            }
            dirty.extend(d_then);
            dirty.extend(d_else);
        },
        // 循环体可能跑多趟，switch 各臂还可能贯穿：先一把收集整棵子树里的写，
        // 下探之前就把这些候选判脏，否则「第二趟读到被改过的值却代回成实参」。
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            let mut w = std::collections::HashSet::new();
            collect_cand_writes_expr(cond, cands, &mut w);
            collect_cand_writes_stmt(body, cands, &mut w);
            dirty.extend(w);
            subst_asm_immediate_ops_in_expr(cond, cands, dirty);
            subst_cand_reads_stmt(body, cands, dirty);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            let mut w = std::collections::HashSet::new();
            if let Some(i) = init {
                collect_cand_writes_stmt(i, cands, &mut w);
            }
            if let Some(c) = cond {
                collect_cand_writes_expr(c, cands, &mut w);
            }
            if let Some(st) = step {
                collect_cand_writes_expr(st, cands, &mut w);
            }
            collect_cand_writes_stmt(body, cands, &mut w);
            dirty.extend(w);
            if let Some(i) = init {
                subst_cand_reads_stmt(i, cands, dirty);
            }
            if let Some(c) = cond {
                subst_asm_immediate_ops_in_expr(c, cands, dirty);
            }
            if let Some(st) = step {
                subst_asm_immediate_ops_in_expr(st, cands, dirty);
            }
            subst_cand_reads_stmt(body, cands, dirty);
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            let mut w = std::collections::HashSet::new();
            collect_cand_writes_expr(expr, cands, &mut w);
            for it in prelude.iter() {
                if let BlockItem::Stmt(st) = it {
                    collect_cand_writes_stmt(st, cands, &mut w);
                }
            }
            for c in cases.iter() {
                collect_cand_writes_items(&c.body.items, cands, &mut w);
            }
            dirty.extend(w);
            subst_asm_immediate_ops_in_expr(expr, cands, dirty);
            for it in prelude {
                if let BlockItem::Stmt(st) = it {
                    subst_cand_reads_stmt(st, cands, dirty);
                }
            }
            for c in cases {
                for bit in &mut c.body.items {
                    if let BlockItem::Stmt(st) = bit {
                        subst_cand_reads_stmt(st, cands, dirty);
                    }
                }
            }
        },
        Stmt::Block { b, .. } => subst_cand_reads_items(&mut b.items, cands, dirty),
        Stmt::Label { stmt, .. } => subst_cand_reads_stmt(stmt, cands, dirty),
        Stmt::Invoke { call, .. } => subst_cand_reads_stmt(call, cands, dirty),
        _ => {},
    }
}

fn subst_cand_reads_items(
    items: &mut [BlockItem],
    cands: &mut HashMap<String, Expr>,
    dirty: &mut std::collections::HashSet<String>,
) {
    // 本块新压入的候选名，离开本块时弹出：代回只在声明所在的作用域内成立。
    // 不这么做就会漏掉「内层同名局部遮蔽外层对象」与「声明之前的读属于另一个
    // 对象（同名的全局）」两种情形 —— 它们都会拿到错值而不是报错。
    let mut pushed: Vec<String> = Vec::new();
    for it in items {
        match it {
            BlockItem::Stmt(s) => subst_cand_reads_stmt(s, cands, dirty),
            BlockItem::Decl(Decl::Var(v)) => {
                // 声明本身先处理（初值在声明之后才有效），再看初值里嵌的 asm。
                // 候选值多接一条「链」：初值本身不是常量，但整体就是一个已知候选的读。
                // 内联嵌套两层以上必然是这个形状：`__inl$_nr = __inl$_bit`，而
                // `__inl$_bit = <caller 的常量实参>`。不接链的话，第二层内联出来的
                // `"i" (1 << (nr & 7))` 永远等不到常量（实测 `kernel/ptrace.o` 的 `wait_on_bit`：
                // `include/linux/wait_bit.h:72` 是 `if (!test_bit_acquire(bit, word)) return 0;`，
                // 而 `bit` 在 caller 里就是个字面量，却因为多了一层临时量而报 E0311）。
                // 只能接「初值恰好是一个 Ident 读」：别的形状里混着候选名不叫常量初值。
                // 存储期/属性那一串排除项与 [`const_foldable_init`] 同口径（`static` 局部
                // 可被别的调用改， TLS 同理），所以这里不能绕过它们。
                let chain_ok = !v.is_static
                    && !v.is_extern
                    && !v.is_thread_local
                    && v.cleanup_fn.is_none()
                    && v.register_name.is_none()
                    && v.asm_label.is_none();
                let init = const_foldable_init(v).or_else(|| {
                    if !chain_ok {
                        return None;
                    }
                    match &v.init {
                        Some(Expr::Ident { name: n, .. }) if !dirty.contains(&n.name) => {
                            cands.get(&n.name).cloned()
                        },
                        _ => None,
                    }
                });
                if let Some(init) = init {
                    let name = v.name.name.clone();
                    if cands.contains_key(&name) {
                        // 遮蔽：两个不同对象共用一个名字，按名字替换分不开 ⇒ 失败关闭。
                        cands.remove(&name);
                        dirty.insert(name);
                    } else if !dirty.contains(&name) {
                        cands.insert(name.clone(), init);
                        pushed.push(name);
                    }
                }
                if let Some(e) = &mut v.init {
                    // 初值里整棵就是 `({ … })` 时（`int ret = ({ …; });`，内联助手常见），
                    // 同样把块交给收集器跑一遍：理由与 [`subst_cand_reads_stmt`] 的表达式
                    // 语句臂一致 —— 块内自己声明的临时量必须在这里被收进来。
                    if let Expr::StmtExpr { block, .. } = e {
                        subst_cand_reads_items(&mut block.items, cands, dirty);
                    }
                    subst_asm_immediate_ops_in_expr(e, cands, dirty);
                    let mut w = std::collections::HashSet::new();
                    collect_cand_writes_expr(e, cands, &mut w);
                    dirty.extend(w);
                }
            },
            _ => {},
        }
    }
    for n in pushed {
        cands.remove(&n);
    }
}

/// 刚被强制内联搬进 caller 的那一串项里，趁早把「条件已是编译期值」的分支剪掉。
///
/// 两步缺一不可，而且必须在递归展开剩下的调用点**之前**做完：
///  1. 形参临时量代回 caller 实参（`subst_cand_reads_items`，与 `fold_asm_immediate_consts`
///     同一判据）：内核一大族 `__always_inline` 助手的体是
///     `return __builtin_constant_p(形参) ? 带 "i" 操作数的写法 : 运行时写法;`
///     （`arch/x86/include/asm/bitops.h:239` 的 `arch_test_bit_acquire`），临时量不代回就
///     折不动那个条件。
///  2. 折掉已定条件的分支。`fold_branches_stmt` 只看 `If`/`While`/`For`/`Block`，不看
///     `Return`/`Expr` 里的三元，所以这里先把这两类的表达式过一遍 `fold_expr`。
/// 不这么做就会看到实测结果：`kernel/sched/wait_bit.o` 的
/// `do { … } while (test_bit_acquire(…) && !ret)` 报
/// `E0311: ... (expression: (1 << (__inl$_nr & 7)), in function __wait_on_bit)` ——
/// 那条根本不会执行的分支被递归展开后带进了 `Stmt::Label`，之后的剪枝因为
/// 「goto 可跳入死分支标签」拒绝工作，死分支里的 `"i"(形参)` 就被当成活代码编了。
fn prune_spliced_items(items: &mut Vec<BlockItem>) {
    let mut cands: HashMap<String, Expr> = HashMap::new();
    let mut dirty = std::collections::HashSet::new();
    subst_cand_reads_items(items, &mut cands, &mut dirty);
    // 代回之后，形参临时量可能已经没人读了；但它的**初始化**还留着，而初值常常是
    // `&some_extern.key` 这样一个外部符号的地址 —— 语句照发，`.text` 里就留下一条指向
    // 外部数据的 `R_X86_64_PC32`。在 `vmlinux` 里这条重定位无害，vDSO 是 `ld -shared`，
    // 直接死在 `relocation against '__tracepoint_write_msr' in read-only section '.text'
    // can not be used when making a shared object`（实测 `vclock_gettime.o`；gcc 那边
    // 这次读早被常量传播消掉，对象里没有这条重定位）。
    drop_dead_inline_temps(items);
    // 与 `fold_constant_branches` 同一口径：死分支折叠要问「臂里的标号在臂外还有没有人跳」，
    // 否则 `asm goto` 的落点会随 `if (0)` 一起被删（`signal_64.c` 的 E1103）。
    // 逐项问 `goto_facts_of_item` 而不克隆整段 items 成临时 `Block`：本函数在每个内联
    // 展开处都跑一次，深拷一整段子树会把开销做成 O(n²)。
    let live_refs: std::collections::HashSet<String> = items
        .iter()
        .flat_map(|it| crate::ast_dispatch::goto_facts_of_item(it).0)
        .collect();
    for it in items.iter_mut() {
        if let BlockItem::Stmt(s) = it {
            match s {
                Stmt::Return { val: Some(e), .. } | Stmt::Expr { e, .. } => {
                    *e = fold_expr(e.clone(), false);
                },
                _ => fold_branches_stmt(s, false, &live_refs),
            }
        }
    }
    fold_branches_block(items, false, &live_refs);
}

/// 删掉「内联器造的、已经没人读的临时量」声明。
///
/// 判据全保守，任一条不满足就保留：
///   * 名字必须是内联器的命名（`__inl<数字>_…`）；
///   * 普通局部（非 static/extern/TLS/register 变量/有别名/带 asm 标签/带 VLA 维）；
///   * **必须有初值**且初值无副作用（没初值的声明本来也不发码）；
///   * 名字在整段 items 里再没出现过 —— [`item_mentions_name`] 已覆盖 asm 输入/输出
///     操作数、语句表达式、复合字面量与初始化器，所以「`"r"(临时量)` 还活着」这类
///     形状不会被误删。
///
/// 逐项循环直到没得删：删掉一项后，它的初值里可能又少了一个引用者。
fn drop_dead_inline_temps(items: &mut Vec<BlockItem>) {
    loop {
        let mut victim: Option<usize> = None;
        for (i, it) in items.iter().enumerate() {
            let BlockItem::Decl(Decl::Var(v)) = it else {
                continue;
            };
            if !v.name.name.starts_with("__inl")
                || v.is_static
                || v.is_extern
                || v.is_thread_local
                || v.register_name.is_some()
                || v.alias_name.is_some()
                || v.asm_label.is_some()
                || type_has_vla_dim(&v.ty)
            {
                continue;
            }
            let Some(init) = &v.init else { continue };
            if crate::semantics::expr_has_side_effects(init) {
                continue;
            }
            let name = v.name.name.clone();
            let used = items
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && item_mentions_name(other, &|n| n == name));
            if !used {
                victim = Some(i);
                break;
            }
        }
        match victim {
            Some(i) => {
                items.remove(i);
            },
            None => break,
        }
    }
}

/// 把「此刻必然是常量」的变量读代进 asm 的只立即数操作数。
///
/// 为什么必须有这一步：`"i"`/`"I"`/`%c[...]` 这类约束在 GCC 里是链接期常量，而两处
/// 形状都拿不到它：
///   * 函数体内联把实参先存进 `__inlN$_x` 临时量（形参可写，不能直接代入），
///     内核 `alternative_msr_write`/`_static_cpu_has`/`arch_static_branch` 的
///     `[feature] "i" (bit)` 于是落在寄存器上，`.altinstructions`/`__jump_table` 条目
///     里就留下指向寄存器名的野重定位（objtool 报 `can't find orig instruction`）；
///   * caller 自己的「写一次的常量局部」，如 `__WARN_bug_entry` 的
///     `int __flags = ...; [fl] "i" (__flags)`。
/// GCC 靠内联后的常量传播把那次「存进临时量/局部再读回」消掉；dcc 的折叠走不到
/// asm 操作数，所以在这里按位置做等价代回。候选必须是常量初值（见
/// [`const_foldable_init`]），被写过就标脏（只有写之前的读享受代回，与 GCC 的
/// 逐点常量同口径）。
///
/// 必须跑在 `function_inline` **之后**、`resync`/codegen 之前；跑在任何 -O 档都算的
/// 位置上（`-O0` 下 `always_inline` 依然会被展开，那些临时量一样需要常量）。
fn fold_asm_immediate_consts(module: &mut Module) {
    let prog = module.source_mut();
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if let Some(body) = &mut f.body {
                let mut cands: HashMap<String, Expr> = HashMap::new();
                let mut dirty = std::collections::HashSet::new();
                // 形参不进候选表：它们的值是运行时的。内联产生的形参临时量是普通的
                // `VarDecl`（初值 = caller 实参），走下面那条按作用域压栈的路。
                subst_cand_reads_items(&mut body.items, &mut cands, &mut dirty);
            }
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
                            BlockItem::Stmt(Stmt::Expr {
                                e,
                                span: span.clone(),
                            }),
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

/// 内联改名的作用域栈。
///
/// 为什么必须是栈而不是一张表：见 [`inline_rename_items`] 的头注。
struct InlineScope {
    entries: Vec<(String, String)>,
}

impl InlineScope {
    /// 从形参映射起手（形参临时量在展开块的开头声明，对整个 callee 体可见）。
    fn from_map(map: &HashMap<String, String>) -> Self {
        Self {
            entries: map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    fn mark(&self) -> usize {
        self.entries.len()
    }

    fn rollback(&mut self, m: usize) {
        self.entries.truncate(m);
    }
}

/// 把 callee 体里的局部声明与引用**一次**改成展开后的唯一名字（带作用域栈）。
///
/// 旧实现是两步走：先把所有层级的声明收进一张 `map`（`rename_decl_names_in_item`），
/// 再用它整棵改引用（`rename_idents_in_items`）。那张表没有作用域概念 —— 内层
/// `{ int x = 5; }` 会把形参 `x` 的表项盖掉，于是同一函数体里**外层**后续的 `x` 也变成
/// 内层那个名字。实测夹具：
///     static __always_inline int shadow(int x) { int r = ({ int x = 5; x + 1; }); return r + x; }
/// 旧实现得 11（`r=6` + 读到内层的 5），gcc 是 13。以前这条只在「嵌套块遮蔽形参」时偶发，
/// 本轮把 `({ … })` 放进了内联准入，它会变成常态，所以必须换成带作用域的单趟改写。
///
/// 唯一性用 `seq`（一次展开内只增）而不是作用域深度：同名局部在不同块里合法，
/// 唯一化后就算作用域判错也不会互串（宁可多一个槽，不要静默错值）。
fn inline_rename_items(items: &mut [BlockItem], uid: u32, seq: &mut u32, scope: &mut InlineScope) {
    for it in items {
        match it {
            // 块作用域的裸 `extern` 声明**不改名**：它命名的就是那个外部符号本身，而
            // `__compiletime_assert_N` 这一类「故意没有定义」的诊断函数更是名字即契约
            // （改了之后链接期的 `undefined reference to __inl0_l__compiletime_assert_N`
            // 完全对不上源码里的 `BUILD_BUG_ON` 位置）。不改名同时意味着调用点也不用改。
            BlockItem::Decl(Decl::Var(v)) if v.is_extern && v.init.is_none() => {},
            BlockItem::Decl(Decl::Var(v)) => {
                *seq += 1;
                let new = format!("__inl{}_l{}_{}", uid, v.name.name, *seq);
                scope.entries.push((v.name.name.clone(), new.clone()));
                v.name.name = new;
                // 初始化式在**新名字入栈之后**改：C 里 `int x = x;` 的初始化式看到的
                // 就是新声明的 `x`（未初始化），与旧实现同口径。
                if let Some(e) = &mut v.init {
                    inline_rename_expr(e, uid, seq, scope);
                }
            },
            // 无体函数声明搬进来只为引一个外部名字，名字本身不能改。
            BlockItem::Decl(Decl::Func(_)) => {},
            BlockItem::Stmt(s) => inline_rename_stmt(s, uid, seq, scope),
            _ => {},
        }
    }
}

fn inline_rename_stmt(s: &mut Stmt, uid: u32, seq: &mut u32, scope: &mut InlineScope) {
    match s {
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
            inline_rename_expr(e, uid, seq, scope)
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            inline_rename_expr(cond, uid, seq, scope);
            inline_rename_stmt(then_, uid, seq, scope);
            if let Some(el) = else_ {
                inline_rename_stmt(el, uid, seq, scope);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            inline_rename_expr(cond, uid, seq, scope);
            inline_rename_stmt(body, uid, seq, scope);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            // `for (int i = …; …)` 的 `i` 只在 for 自己的作用域里有效。
            let m = scope.mark();
            if let Some(i) = init {
                // `init` 在 parser 里一律是 `Stmt::Block{…}`（实测 `for (int i = 0;…)` 形状）。
                // **不能**走 Block 臂：那一支会自己压栈、改完再回滚，于是 `i` 的新名字出了
                // init 就失效 —— 条件与步进仍读外层同名变量（实测 tile_strip_order_break：
                // `for (int i…; i < 6; i++)` 的条件恒假，循环体一次不跑）。
                match &mut **i {
                    Stmt::Block { b, .. } => inline_rename_items(&mut b.items, uid, seq, scope),
                    other => inline_rename_stmt(other, uid, seq, scope),
                }
            }
            if let Some(c) = cond {
                inline_rename_expr(c, uid, seq, scope);
            }
            if let Some(st) = step {
                inline_rename_expr(st, uid, seq, scope);
            }
            inline_rename_stmt(body, uid, seq, scope);
            scope.rollback(m);
        },
        Stmt::Block { b, .. } => {
            let m = scope.mark();
            inline_rename_items(&mut b.items, uid, seq, scope);
            scope.rollback(m);
        },
        // `switch` 的 `prelude` 与所有 `case` 体属**同一个**作用域，不压栈。
        Stmt::Switch {
            expr,
            prelude,
            cases,
            ..
        } => {
            inline_rename_expr(expr, uid, seq, scope);
            inline_rename_items(prelude, uid, seq, scope);
            for c in cases.iter_mut() {
                if let Some(v) = &mut c.val {
                    inline_rename_expr(v, uid, seq, scope);
                }
                inline_rename_items(&mut c.body.items, uid, seq, scope);
            }
        },
        // 标签名不在这里改（`prefix_labels_in_stmt` 负责）；只走它包着的语句。
        Stmt::Label { stmt, .. } => inline_rename_stmt(stmt, uid, seq, scope),
        // asm 语句的操作数是普通 C 表达式，形参改名后必须跟着改，否则读到 caller 的同名变量。
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for e in outputs.iter_mut().chain(inputs.iter_mut()) {
                inline_rename_expr(e, uid, seq, scope);
            }
        },
        Stmt::Invoke { call, .. } => inline_rename_stmt(call, uid, seq, scope),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            inline_rename_expr(object, uid, seq, scope);
            inline_rename_expr(selector, uid, seq, scope);
        },
        _ => {},
    }
}

fn inline_rename_expr(e: &mut Expr, uid: u32, seq: &mut u32, scope: &mut InlineScope) {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(new) = scope.lookup(&name.name) {
                name.name = new;
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            inline_rename_expr(l, uid, seq, scope);
            inline_rename_expr(r, uid, seq, scope);
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
        | Expr::AlignofExpr { e: x, .. }
        | Expr::Member { expr: x, .. }
        | Expr::VaArg { ap: x, .. } => inline_rename_expr(x, uid, seq, scope),
        Expr::Index { arr, idx, .. } => {
            inline_rename_expr(arr, uid, seq, scope);
            inline_rename_expr(idx, uid, seq, scope);
        },
        Expr::Call { callee, args, .. } => {
            inline_rename_expr(callee, uid, seq, scope);
            for a in args {
                inline_rename_expr(a, uid, seq, scope);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            inline_rename_expr(cond, uid, seq, scope);
            inline_rename_expr(then_, uid, seq, scope);
            inline_rename_expr(else_, uid, seq, scope);
        },
        Expr::AsmExpr {
            inputs, outputs, ..
        } => {
            for o in inputs.iter_mut().chain(outputs.iter_mut()) {
                inline_rename_expr(o, uid, seq, scope);
            }
        },
        Expr::Offsetof { indices, .. } => {
            for i in indices.iter_mut() {
                inline_rename_expr(i, uid, seq, scope);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits.iter_mut() {
                match it {
                    InitItem::Expr(x) => inline_rename_expr(x, uid, seq, scope),
                    InitItem::Designated { value, .. } => {
                        inline_rename_expr(value, uid, seq, scope)
                    },
                }
            }
        },
        // 语句表达式自成一个块作用域：它就压在栈上，出去时弹出。
        Expr::StmtExpr { block, .. } => {
            let m = scope.mark();
            inline_rename_items(&mut block.items, uid, seq, scope);
            scope.rollback(m);
        },
        // 字面量/sizeof/alignof/`&&label`：不含可重命名的对象名。
        _ => {},
    }
}

/// 展开后的 callee 返回值要送去哪里。阶段5 只接了前两种，于是
/// `rc = fillInCell(...)`（btree 四个热函数）与 `L->ci = ci = prepCallInfo(...)`
/// （Lua 的 prepCallInfo，在 fib(32) 里占 6.8%）**从未进入过任何内联决策点**：
/// 形状匹配不上就直接 `return false`，连 `DCC_INLINE_TRACE` 的 REJECT 行都不打，
/// 只看日志的人会以为剩下的只有成本问题。
#[derive(Clone)]
enum Dest {
    /// 值被丢弃：`f(...);`
    Discard,
    /// 尾位置：`return f(...);`（callee 的 `return` 就是本层的 `return`）
    ReturnValue,
    /// 单层赋值：`x = f(...);`
    Assign(Expr),
    /// 链式赋值：`y = x = f(...);`（先写内层左值再写外层，两者得到同一个值）
    AssignChain { inner: Expr, outer: Expr },
    /// 整个条件就是调用：`if (f(...)) … else …`（cond 换成结果临时量）
    Cond { then_: Stmt, else_: Option<Stmt> },
    /// 复合赋值 `x op= f(...)` 的脱糖形态（`x = f(...) op x` 或 `x = x op f(...)`）。
    ///
    /// 展开只动一处：把那一侧的 **Call 换成结果临时量的读**，另一侧的左值读表达式
    /// 原样搬回来。所以读-改-写的次序与未展开时逐字相同 —— 上一版注释说「要重讲次序」，
    /// 其实不需要：这里不是重新决定次序，而是同一棵树上少一个调用。
    CompoundAssign {
        target: Expr,
        op: BinOp,
        /// 调用在 Binop 的哪一侧。parser 对**可交换整型**运算会把两侧对调（让 RHS 先求值，
        /// 与 gcc 一致），所以两种顺序都得认。
        call_on_left: bool,
        /// 与 `target` 结构相等的那一侧（原样搬回，不动它）
        lvalue_read: Expr,
        /// 内层 Binop 与外层 Assign 各自的类型（窄类型的 `binop_ty` 是 `int`，不能重算）
        binop_ty: Type,
        assign_ty: Type,
    },
}

/// 把一条语句认成「可展开的调用点」，返回 (callee 名, 实参, 结果去向)。
///
/// 只认整条语句就是那个形状的情形；嵌在更大表达式里的（`a = f() + g()`、
/// `c ? f() : g()`）仍不接 —— 那需要语句表达式包装，把整棵式子拖进临时量会多出
/// 「求值次数/次序」的账。两条已经接进来但要看 `DCC_INLINE_SHAPE`：`x op= f(...)`
/// （见 `Dest::CompoundAssign`，它只替换子表达式，不改次序）与声明初值（在
/// `inline_calls_in_block` 那侧处：它把 `T x = f(...)` 当 `x = f(...)` 展，先留个空声明）。
fn call_site_shape(s: &Stmt) -> Option<(String, Vec<Expr>, Dest)> {
    let call_of = |e: &Expr| -> Option<(String, Vec<Expr>)> {
        match e {
            Expr::Call {
                callee,
                args,
                span: _,
                ty: _,
            } => match callee.as_ref() {
                // 只接直接调用（`Expr::Ident`  callee）：函数指针调用的 callee 本身可能是
                // 一个要求值的表达式，搬进临时量会改变求值次数。
                Expr::Ident { name, .. } => Some((name.name.clone(), args.clone())),
                _ => None,
            },
            _ => None,
        }
    };
    // 普通赋值在本 AST 里就是 `Assign{op: BinOp::Add}`（复合赋值早已被 parser 脱糖成
    // `r` 里的一棵 Binop），所以「r 恰好是一个 Call」这一条判据就把 `x op= f()` 天然排除
    // 了——不需要另开一份「读-改-写次序」的讨论。
    let plain_assign = |e: &Expr| -> Option<(Box<Expr>, Box<Expr>)> {
        match e {
            Expr::Assign {
                op: BinOp::Add,
                l,
                r,
                ..
            } => Some((l.clone(), r.clone())),
            _ => None,
        }
    };
    match s {
        // 一条表达式语句：可能是「值被丢弃的调用」，也可能是「赋值」。两个形状要在
        // 同一个臂里分（写两个 `Stmt::Expr` 臂会让后面那个永远不可达）。
        Stmt::Expr { e, .. } => {
            if let Some((n, a)) = call_of(e) {
                return Some((n, a, Dest::Discard));
            }
            // `x = f(...)`。
            let (l, r) = plain_assign(e)?;
            if let Some((n, a)) = call_of(&r) {
                return Some((n, a, Dest::Assign(*l)));
            }
            // `y = x = f(...)`（Lua 的 prepCallInfo 就是这一形）。
            if let Some((inner_l, inner_r)) = plain_assign(&r) {
                if let Some((n, a)) = call_of(&inner_r) {
                    return Some((
                        n,
                        a,
                        Dest::AssignChain {
                            inner: *inner_l,
                            outer: *l,
                        },
                    ));
                }
            }
            // `x op= f(...)`：parser 把它脱糖成 `Assign{Add, target, Binop{op, …, …}}`，
            // 其中一侧就是那个调用、另一侧与 target 结构相等。两种侧序都要认（可交换
            // 整型运算被 parser 对调过）。上一行不能再用 `plain_assign(&r)?` 的 `?` 提前退出：
            // 这一形的 `r` 是一棵 Binop，不是 Assign，那会在判据之前就 bail。
            // 闸门关着时不接这一形（回到原样：调用仍是调用）。
            if inline_shape_enabled() {
                if let Expr::Binop {
                    op,
                    l: bl,
                    r: br,
                    ty: binop_ty,
                    ..
                } = r.as_ref()
                {
                    // 恰一侧是 Call（另一侧必是那个左值读）；两侧都是 Call 时不是 `op=` 的脱糖。
                    let (n, a, call_on_left, lvalue_read) = match (call_of(bl), call_of(br)) {
                        (Some((n, a)), None) => (n, a, true, br.as_ref().clone()),
                        (None, Some((n, a))) => (n, a, false, bl.as_ref().clone()),
                        _ => return None,
                    };
                    // 「另一侧与 target 结构相等」是这条判据的全部：不相等就不是 `op=`。
                    if crate::semantics::exprs_structurally_equal(&lvalue_read, l.as_ref()) {
                        let assign_ty = l.ty();
                        return Some((
                            n,
                            a,
                            Dest::CompoundAssign {
                                target: *l,
                                op: *op,
                                call_on_left,
                                lvalue_read,
                                binop_ty: binop_ty.clone(),
                                assign_ty,
                            },
                        ));
                    }
                }
            }
            None
        },
        Stmt::Return { val: Some(v), .. } => match call_of(v) {
            Some((n, a)) => Some((n, a, Dest::ReturnValue)),
            None => None,
        },
        // `if (f(...)) …`：条件整体就是一个调用，因此把求值提到 `if` 之前不改变
        // 任何可观察顺序（短路组合里嵌的调用不属于这一条，那里 cond 不是 Call）。
        Stmt::If {
            cond, then_, else_, ..
        } => {
            let (n, a) = call_of(cond)?;
            Some((
                n,
                a,
                Dest::Cond {
                    then_: (**then_).clone(),
                    else_: else_.as_ref().map(|e| (**e).clone()),
                },
            ))
        },
        _ => None,
    }
}

/// callee 的 `return` 改写。`rv = Some(name)` 时先把值存进那个临时量再跳出；
/// `None` 就是旧的「值丢弃」形态（只求值不存）。
///
/// 返回值：**本次递归是否真的改写掉至少一条 `Stmt::Return`**。调用方用它决定要不要
/// 发那条「展开块出口」标签：只有真有人 `goto` 它，标签才有意义。
/// 这件事必须在本函数（唯一的 `goto` 生产者）里报出去，而不是在下游再加一次「扫一遍
/// 有没有人引用这个标签」的判定 —— 后者就是本仓库反复付过学费的「同一个规则两份实现」。
/// 为什么少发一条死标签要紧：`ssa/structure.rs::stmt_has_unstructured_flow` 把
/// `Stmt::Label` 一律算作非结构化，而 `run_ssa_passes` 对非结构化函数只走
/// analysis-only 分支（不回写 AST）⇒ 一条**没人引用**的合成标签会把整个函数踢出
/// SCCP/重建通道，连带它的常量 `switch` 永远折不掉。实测：拼接一个体内无 `return`
/// 的 `static inline __always_inline` 助手（内核那族 `switch (size)` 尺寸检查宏）后，
/// callee 形参的临时量作为选择子就再也回代不成常量，产物里留下对「故意不定义」
/// 的陷阱函数的重定位，最终 `ld vmlinux` 报 `undefined reference to '__cmpxchg_wrong_size'`。
fn rewrite_returns_to_dest(
    s: &mut Stmt,
    end_label: &str,
    rv: Option<&str>,
    rv_ty: Option<&Type>,
    span: &Span,
) -> bool {
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
            let step: Option<Stmt> = match (val, rv) {
                (Some(e), Some(name)) => {
                    // 左值类型必须是**结果槽的声明类型**（callee 的 `f.ret_ty`），不是返回
                    // 表达式的类型：`char *scan(...) { … return scratch; }` 里 `scratch`
                    // 是 `char[16]`，照抄 `e.ty()` 会把这次写入变成一次 16 字节**聚合拷贝**，
                    // `__inl$rv` 里躺的是数组内容而不是它的地址（实测 `wrap(v)` 返回
                    // `0x…656c70`，即 "plain-te" 的字节）。右值保持原类型：数组→指针的
                    // 退化与整数提升由既有的赋值路径补上（`p = arr;` 本来就走这条路）。
                    let ety = e.ty();
                    let lty = rv_ty.cloned().unwrap_or_else(|| ety.clone());
                    Some(Stmt::Expr {
                        e: Expr::Assign {
                            op: BinOp::Add,
                            l: Box::new(Expr::Ident {
                                name: Ident {
                                    name: name.to_string(),
                                    span: span.clone(),
                                },
                                span: span.clone(),
                                ty: lty.clone(),
                            }),
                            r: Box::new(e),
                            span: span.clone(),
                            ty: lty,
                        },
                        span: span.clone(),
                    })
                },
                (Some(e), None) => Some(Stmt::Expr {
                    e,
                    span: span.clone(),
                }),
                (None, _) => None,
            };
            let mut items: Vec<BlockItem> =
                step.into_iter().map(BlockItem::Stmt).collect::<Vec<_>>();
            items.push(BlockItem::Stmt(goto));
            *s = Stmt::Block {
                b: Block {
                    items,
                    span: span.clone(),
                },
                span: span.clone(),
            };
            true
        },
        Stmt::If { then_, else_, .. } => {
            let mut rewrote = rewrite_returns_to_dest(then_, end_label, rv, rv_ty, span);
            if let Some(e) = else_ {
                rewrote |= rewrite_returns_to_dest(e, end_label, rv, rv_ty, span);
            }
            rewrote
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
            rewrite_returns_to_dest(body, end_label, rv, rv_ty, span)
        },
        Stmt::Block { b, .. } => {
            let mut rewrote = false;
            for it in b.items.iter_mut() {
                if let BlockItem::Stmt(st) = it {
                    rewrote |= rewrite_returns_to_dest(st, end_label, rv, rv_ty, span);
                }
            }
            rewrote
        },
        Stmt::Switch { cases, prelude, .. } => {
            // prelude 也是 callee 体的一部分（`switch (x) { return 5; case 1: … }` 里那个
            // return 虽然今天不可达，但同一文件里另四个遍历器都走 prelude）—— 不递归进去
            // 就会把 callee 的 `return` 原样带进 caller，变成 caller 的 return。
            let mut rewrote = false;
            for it in prelude.iter_mut() {
                if let BlockItem::Stmt(st) = it {
                    rewrote |= rewrite_returns_to_dest(st, end_label, rv, rv_ty, span);
                }
            }
            for c in cases.iter_mut() {
                for it in c.body.items.iter_mut() {
                    if let BlockItem::Stmt(st) = it {
                        rewrote |= rewrite_returns_to_dest(st, end_label, rv, rv_ty, span);
                    }
                }
            }
            rewrote
        },
        Stmt::Label { stmt, .. } => rewrite_returns_to_dest(stmt, end_label, rv, rv_ty, span),
        _ => false,
    }
}

/// 左值根：沿 `Member`/`Index`/`Cast` 往下找到那个对象名（`s.f`、`a[i]`、`(T)x` 的根
/// 分别是 `s`/`a`/`x`）。**不穿过 `Deref`**：`*p = 1` 写的是 p 指向的对象，不是 p。
fn expr_lvalue_root_name(x: &Expr) -> Option<&str> {
    let mut cur = x;
    loop {
        match cur {
            Expr::Ident { name, .. } => return Some(name.name.as_str()),
            Expr::Member { expr, .. }
            | Expr::Index { arr: expr, .. }
            | Expr::Cast { e: expr, .. } => cur = expr,
            _ => return None,
        }
    }
}

/// 操作数约束是否**取对象的地址**：内存形式（`m`/`o`/`V`）就是地址，`g`/`X` 允许汇编
/// 挑内存形式。修饰前缀（`=`/`+`/`&`/`%`/匹配数字）只说明「怎么写」，不影响这个判断。
fn asm_constraint_takes_address(c: &str) -> bool {
    c.chars().any(|ch| matches!(ch, 'm' | 'o' | 'V' | 'g' | 'X'))
}

/// 体内 `asm`/`asm goto` 会**写**或**取地址**的名字（左值根）。
///
/// 为什么值得单独扫：§5.39 的「形参直绑实参名」要求形参在体内只读不取址；而 `asm`
/// 输出操作数（`"=r"(p)` / `"+m"(p)`）写的就是形参对象本身，直绑之后写穿到调用者的
/// 变量 —— `mm/memblock.c::__free_pages_memory` 里 `__ffs(start)` 就是这么把页游标
/// 改成 0 的（`tzcnt %rcx,%rax` 的结果落回 `start` 的槽，每轮都在释放同一页，boot
/// 停在 `mem auto-init` 之后），见报告 §5.41。
///
/// `asm goto` 的操作数挂在 `Stmt` 上，`walk_block_ref` 交出来的只是操作数内部的表达
/// 式，`"哪个操作数、什么约束"` 这层信息已经丢了 —— 所以借 [`scan_block`] 那份穷举
/// 语句递归（连语句表达式里的块一起走）一次收全，而不是再写一份同样的遍历。
fn asm_written_roots_of(body: &Block) -> Vec<String> {
    let mut acc = InlineScan::default();
    scan_block(body, &mut acc);
    acc.asm_written_roots
}

/// `asm` 操作数里哪些名字会被写/取址（见 [`InlineScan::asm_written_roots`]）：
/// 输出操作数恒写；输入操作数只在约束允许内存形式时才算取址。
fn note_asm_operand_roots(
    outputs: &[Expr],
    inputs: &[Expr],
    input_constraints: &[String],
    acc: &mut InlineScan,
) {
    for x in outputs {
        if let Some(n) = expr_lvalue_root_name(x) {
            acc.asm_written_roots.push(n.to_string());
        }
    }
    for (i, x) in inputs.iter().enumerate() {
        if input_constraints
            .get(i)
            .is_some_and(|c| asm_constraint_takes_address(c))
        {
            if let Some(n) = expr_lvalue_root_name(x) {
                acc.asm_written_roots.push(n.to_string());
            }
        }
    }
}

/// 形参 `pname` 在 `body` 里是否**被写、被取地址、被自增自减**。
///
/// 判据只看这四类节点（父节点本身也在 `walk_block_ref` 的遍历里，所以不需要上
/// 下文）：`Assign { l }` 与 `Inc/Dec { e }` 的**左值根**是形参，或 `AddrOf { e }`
/// 的根是形参。左值根沿 `Member`/`Index`/`Cast` 往下找（`s.f = 1`、`a[i] = 1` 都
/// 算写形参副本），但**不穿过 `Deref`**：`*p = 1` 写的是 p 指向的对象，不是 p。
///
/// 为什么保守到连「写成员」都算：直绑之后形参就是实参本身，写副本与写实参的
/// 可见性不同（实参变化会被 caller 看到），那是语义变更而不是优化。
///
/// `asm` 操作数不在这条遍历里（`Stmt::AsmGoto` 的操作数挂在语句节点上，见
/// [`asm_written_roots_of`]）：调用方要把两份结果一起看。
fn param_written_or_addressed(body: &Block, pname: &str) -> bool {
    let mut hit = false;
    let root_is_param = |x: &Expr| expr_lvalue_root_name(x) == Some(pname);
    walk_block_ref(body, &mut |e| {
        if hit {
            return;
        }
        match e {
            Expr::Assign { l, .. } => {
                if root_is_param(l) {
                    hit = true;
                }
            },
            Expr::AddrOf { e: x, .. }
            | Expr::PreInc { e: x, .. }
            | Expr::PreDec { e: x, .. }
            | Expr::PostInc { e: x, .. }
            | Expr::PostDec { e: x, .. } => {
                if root_is_param(x) {
                    hit = true;
                }
            },
            _ => {},
        }
    });
    hit
}

/// P13 阶段5：-O2 起对**小 `static` 函数**做函数体级内联（允许控制流）。
///
/// 为什么单独一条臂而不是改 `always_inline` 那条：那条的准入是「程序员明说」，
/// 本条是「成本模型推断」，两者错了的代价完全不同（前者是用户表达错了，后者是
/// 编译器猜错了），判据不能合在一起改。
///
/// 阶段6 批3 接住了原来漏掉的调用点形状（见 `call_site_shape`）：除了「值被丢弃的
/// 表达式语句」与「尾位置 `return f(...)`」，现在还能展开 `x = f(...)`、
/// `y = x = f(...)` 与 `if (f(...))`；后三类需要一个结果临时量，它的声明与「出口标签
/// 之后才写目的地」的次序就是本函数下半部分的内容。仍然不接：嵌在更大表达式里的
/// 调用（需要语句表达式包装）、`x op= f(...)`（callee 可能改掉 `x`，读-改-写次序要
/// 重讲）、实参里带调用的点，以及返回 void 却被赋值的情形。
fn try_static_inline_general(
    s: &mut Stmt,
    funcs: &InlineTable,
    level: u8,
    depth: u32,
    allow_effectful_args: bool,
) -> bool {
    let (callee_name, args, dest) = match call_site_shape(s) {
        Some(x) => x,
        None => return false,
    };
    // 报障名单：点名的 callee 一律不展开（见 [`inline_not_listed`]）。
    if inline_not_listed(&callee_name) {
        trace_inline_decision(&callee_name, "REJECT", "DCC_INLINE_NOT 点名否决");
        return false;
    }
    let f = match funcs.get(&callee_name) {
        Some(f) => f,
        None => return false,
    };
    // 档位闸门只拦「投机内联」：成本模型 `-O2` 起才参与。`__always_inline` 是语义
    // 要求，任何档位都得展开（第二轮强制内联传 level 0，见 [`inline_forced_call_as_stmt_expr`]）。
    if level < 2 && !f.is_always_inline {
        return false;
    }
    // 深度闸门：“强制内联”另有更高的线（[`INLINE_MAX_DEPTH_FORCED`]），
    // 不共用投机的 8 层 —— 撞线的代价不是少一个优化，而是多留一份带
    // `switch (形参) { default: BUILD_BUG(); }` 的外带副本（实测 `mm/memory.o`）。
    // 所以判据得在拿到 `f`、知道是否强制**之后**才能下（以前写在函数头，强制内联
    // 也吃 8 层）。
    let depth_limit = if f.is_always_inline {
        INLINE_MAX_DEPTH_FORCED
    } else {
        INLINE_MAX_DEPTH
    };
    if depth >= depth_limit {
        trace_inline_decision(
            &callee_name,
            "REJECT",
            &format!("stmt-inline depth {} >= {depth_limit}", depth),
        );
        return false;
    }
    // 准入：内部链接、没被明说禁止、不是那些已有专属路径的形状。
    // `is_always_inline` 以前也在这一列（「那是专属两条路的事」），而专属两条
    // 一个要「体恰好是一条 `return 表达式;`」（`try_inline_call`），一个要「体是
    // [声明]* [表达式语句]* [return?] 且调用点是表达式语句/尾位置」（`try_stmt_inline_call`）。
    // 带控制流的 `always_inline`（SQLite btree 的 `allocateSpace`，`SQLITE_INLINE` 展开就是
    // `__attribute__((always_inline)) inline`）与 `rc = allocateSpace(...)` 这种赋值调用点
    // 两边都不接 ⇒ 用户的明说反而把机会彻底关门（阶段 6 §15.8-4 的三条路互斥）。
    // 现在只拒 `noinline`，always_inline 走到下面再按「专属路能不能接」让位。
    // 闸门关着时退回本轮之前的形状（`always_inline` 整体不进通用路）。
    if f.is_always_inline && !inline_walk_enabled() {
        trace_inline_decision(&callee_name, "REJECT", "static-inline: DCC_INLINE_WALK 闸门关着");
        return false;
    }
    if !f.is_static
        || f.is_noinline
        || f.is_variadic
        || f.returns_twice
        || f.is_nested
        || f.is_old_style
        || f.alias_name.is_some()
        // 被内联汇编文本点名的 `static`：展开掉就没有人再引用它，定义会消失，
        // 而 asm 文本里的 `call leaf` 还在 ⇒ 链接期 undefined reference。
        || funcs.named_by_asm(&callee_name)
    {
        trace_inline_decision(
            &callee_name,
            "REJECT",
            "static-inline: 准入 (非 static/noinline/变参/returns_twice/嵌套/别名/被 asm 点名)",
        );
        return false;
    }
    if f.params.len() != args.len() {
        trace_inline_decision(&callee_name, "REJECT", "static-inline: 实参个数与形参不等");
        return false;
    }
    for p in &f.params {
        if p.name.is_none() || p.vla_dim.is_some() || type_has_vla_dim(&p.ty) {
            trace_inline_decision(&callee_name, "REJECT", "static-inline: 形参无名或带 VLA 维度");
            return false;
        }
    }
    let body = match &f.body {
        Some(b) => b,
        None => {
            trace_inline_decision(&callee_name, "REJECT", "static-inline: 无函数体（声明）");
            return false;
        },
    };
    // 让位给专属路径：调用点是「整条表达式语句」或「尾位置」且体是一条 `return 表达式;`
    // 时，`try_inline_call` 的逐表达式替换能把形状做得更贴源码（那是既有 -O0 产物与
    // 反汇编快照的口径），本条插进去只会多出标签与 goto。
    if f.is_always_inline
        && matches!(s, Stmt::Expr { .. } | Stmt::Return { .. })
        && matches!(body.items.as_slice(), [BlockItem::Stmt(Stmt::Return { val: Some(_), .. })])
    {
        return false;
    }
    if !body_is_splicable(body) {
        trace_inline_decision(
            &callee_name,
            "REJECT",
            "static-inline: 体不可整体搬移 (computed goto/&&label/嵌套函数/static 局部等)",
        );
        return false;
    }
    // 递归（包括间接递归）一律不展开：不展开不会错，展开会爆栈。
    if calls_graph_reaches(funcs, &callee_name, &callee_name) {
        trace_inline_decision(&callee_name, "REJECT", "static-inline: 递归调用闭包");
        return false;
    }
    let cost = match stmt_splice_cost_ok(funcs, &callee_name, f, body, &args) {
        Ok(c) => c,
        Err(InlineCostReject::Cost {
            cost,
            limit,
            sites,
        }) => {
            trace_inline_decision(
                &callee_name,
                "REJECT",
                &format!("static-inline cost {cost} > {limit} ({sites} 个调用点)"),
            );
            return false;
        },
        Err(InlineCostReject::LargeFrame { bytes, sites }) => {
            trace_inline_decision(
                &callee_name,
                "REJECT",
                &format!(
                    "static-inline 大局部对象 {bytes} B > {INLINE_LARGE_LOCAL_BYTES} B \
                     （栈帧闸门，{sites} 个调用点；gcc --param large-stack-frame）"
                ),
            );
            return false;
        },
    };
    let rspan: Span = match s {
        Stmt::Expr { span, .. } | Stmt::Return { span, .. } | Stmt::If { span, .. } => span.clone(),
        _ => return false,
    };
    // 需要结果的形状要先声明一个结果临时量；callee 返回 void 时这形状本身就不合法，直接不接。
    let needs_rv = matches!(
        dest,
        Dest::Assign(_)
            | Dest::AssignChain { .. }
            | Dest::Cond { .. }
            | Dest::CompoundAssign { .. }
    );
    if needs_rv && matches!(f.ret_ty, Type::Void) {
        return false;
    }
    // 实参里有副作用（调用/自增/赋值）就跳过：实参在 caller 作用域求值，展开后它会被
    // 摆进形参临时量的声明里；而嵌套的展开（实参里又是一个可内联的调用）要递归多一层
    // 同步，本轮不开这个口子。用 `semantics::expr_has_side_effects` 而不是自己写的
    // 版本：调用/赋值/自增一并挡掉，宁可少内联不引入新的求值次数问题。
    //
    // 例外（`allow_effectful_args`）：调用点来自 [`inline_forced_call_as_stmt_expr`]，
    // 那次下探已经把实参里的强制内联调用展开完（`inline_expr` 的「内→外」重试），
    // 剩下的「副作用」只是原本就在的普通调用。形参临时量仍然**只对实参求值一次、
    // 按声明的从左到右次序**（C 的实参求值序本就是 unspecified，固定成这一种合法），
    // 展开前后求值次数逐字相同，所以这里不再拿它否决 —— 否则 `u32_encode_bits(v,
    // GENMASK(...))` 在 `__ffs(links)` 这种实参上会两边都不接，留一份外带副本，
    // 副本体里的 `__bad_mask()` 成为链接期未定义引用（实测 `net/mac80211/tx.o`）。
    if needs_rv && !allow_effectful_args && args.iter().any(crate::semantics::expr_has_side_effects) {
        trace_inline_decision(&callee_name, "REJECT", "static-inline: 实参有副作用");
        return false;
    }

    let uid = STMT_INLINE_UID.fetch_add(1, Ordering::Relaxed);
    let label_prefix = format!("__inl{}__l", uid);
    let end_label = format!("__inl{}__end", uid);
    let mut map: HashMap<String, String> = HashMap::new();
    let mut bind: Vec<BlockItem> = Vec::new();
    // 展开体里**一次都没出现过**的形参名：绑定语句就是纯死存储，但它的初值表达式
    // 会把「取地址」这类无副作用、却带重定位的实参留在产物里。内核里最典型的一例是
    // `set_dma_ops(dev, &dma_dummy_ops)`（include/linux/dma-map-ops.h）：CONFIG_DMA_OPS=n
    // 时那个 inline 的体是空的，gcc 连实参求值一起删；dcc 却留下 `lea dma_dummy_ops`
    // ⇒ drivers/acpi/scan.o 多一条 undefined reference（kernel/dma/dummy.c 根本没编进来）。
    // 有副作用的实参照旧绑定：调用/自增/赋值都得求值，那是 C 的语义。
    let mut used_names: HashSet<String> = HashSet::new();
    walk_block_ref(body, &mut |e| {
        if let Expr::Ident { name, .. } = e {
            used_names.insert(name.name.clone());
        }
    });
    // 形参 → 临时量。不用「把实参表达式直接代入引用处」是因为参数可写（形参是
    // 局部变量），代入会改变求值次数与写入目标；先存后改与 callee 语义逐字一致。
    //
    // **实参表达式属于 caller 作用域，绝不能跟着改名**：否则 `f_luaopen(L)` 里
    // `stack_init(L)` 的实参 `L` 会被改成 `__inlN$_L`（那是刚要新建的形参临时量本身）。
    // 旧代码里 `bind` 与 callee 自己的声明混在同一个 `items` 里一起改名，同一个坑。
    //
    // `asm` 会写/取址的名字一次收全，给下面每个形参的直绑判据共用
    // （[`asm_written_roots_of`]）。
    let asm_written = asm_written_roots_of(body);
    for (p, a) in f.params.iter().zip(args.iter()) {
        let pname = match p.name.as_ref() {
            Some(id) => id.name.clone(),
            None => return false,
        };
        if !used_names.contains(&pname) && !crate::semantics::expr_has_side_effects(a) {
            continue;
        }
        // §5.39：实参就是**同类型的一个左值**、且形参在体内只读不取址时，把形参
        // 直接绑到实参名上，不建 `__inlN$_p` 临时量。
        //
        // 为什么值得（这是 `var_bytes` 膨胀的主源之一）：临时量是一个真正的
        // `Decl::Var`，`pre_register_vars` 会给它留一个 8B 槽 —— 即使它后来拿到了
        // 寄存器家（`ssa::regalloc::deliver`），_ 槽依旧占着帧。`__schedule` 实测
        // var=776 B（97 槽，帧 992 B）里绝大多数是 `__inlN$_rq` / `__inlN$_rf`
        // 这一类只读形参绑定，而 gcc 对同一函数是 96 B。16 KB 内核栈上，25 层
        // early-boot 链因此打穿 guard page（报告 §5.39）。
        //
        // 三个前提都是必要的：实参无副作用（已经由上面那句保证）、**类型相同**
        // （否则直绑会跳过形参处那次隐式转换，`f(long)` 传 `int` 就是错值）、
        // 形参只读不取址（否则形参副本语义与实参别名不同；`asm` 输出操作数与内存
        // 约束的输入同样算写/取址，见 [`asm_written_roots_of`]）。
        //
        // 还有一条防环：实参名不得是**另一个形参名**（`f(b, a)` 直绑成
        // `{a: b, b: a}` 就是一个互换环，一次遍历改名会把两个名字对调回去）。
        // 同名（`rq_lock(rq, &rf)` 里 `rq -> rq`）不是环，放行。
        if let Expr::Ident { name: an, .. } = a {
            let cycle = an.name != pname
                && f.params
                    .iter()
                    .any(|q| q.name.as_ref().is_some_and(|n| n.name == an.name));
            if a.ty() == p.ty
                && !cycle
                && !param_written_or_addressed(body, &pname)
                && !asm_written.iter().any(|n| n == &pname)
            {
                map.insert(pname, an.name.clone());
                continue;
            }
        }
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
    // 结果临时量：只在需要把 callee 的返回值送去某地时才建。名字带 uid 且进 `map`，
    // 所以 callee 体内不可能有一个同名局部被误改（形参/局部都已按 map 改名）。
    let rv_name = if needs_rv {
        format!("__inl{}$rv", uid)
    } else {
        String::new()
    };
    if needs_rv {
        bind.push(BlockItem::Decl(Decl::Var(plain_var_decl(
            f.ret_ty.clone(),
            rv_name.clone(),
            None,
            &rspan,
        ))));
    }
    if !matches!(dest, Dest::ReturnValue) {
        // 值要么丢弃、要么存进结果临时量；两种都要把 `return` 改成「先做完这一步
        // 再跳到展开块出口」。
        let rv: Option<&str> = if needs_rv { Some(rv_name.as_str()) } else { None };
        let mut rewritten: Vec<BlockItem> = Vec::new();
        // 只有真改写掉过 `return`（= 真有人 `goto` 出口）才需要发那条出口标签。
        // 多发一条无人引用的 `Stmt::Label` 看着无害，实际上会让整个 callee 函数在
        // `run_ssa_passes` 里被归为「非结构化」而走 analysis-only 分支（不回写 AST），
        // 连带它的常量 switch / 死分支进不了 SCCP —— 详见 [`rewrite_returns_to_dest`] 的头注。
        let mut has_exit_goto = false;
        for it in moved.drain(..) {
            match it {
                BlockItem::Stmt(mut st) => {
                    has_exit_goto |= rewrite_returns_to_dest(&mut st, &end_label, rv, Some(&f.ret_ty), &rspan);
                    rewritten.push(BlockItem::Stmt(st));
                },
                other => rewritten.push(other),
            }
        }
        moved = rewritten;
        if has_exit_goto {
            moved.push(BlockItem::Stmt(Stmt::Label {
                name: Ident {
                    name: end_label.clone(),
                    span: rspan.clone(),
                },
                stmt: Box::new(Stmt::Block {
                    b: Block {
                        items: vec![],
                        span: rspan.clone(),
                    },
                    span: rspan.clone(),
                }),
                span: rspan.clone(),
            }));
        }
    }
    prefix_labels_in_items(&mut moved, &label_prefix);
    // 声明与引用**一次**改完，带作用域栈：旧的两步走（先收表、再整棵改引用）在没有作用域
    // 概念时会把「内层同名局部」的表项盖到外层引用上，详见 [`inline_rename_items`]。
    {
        let mut seq = 0u32;
        let mut scope = InlineScope::from_map(&map);
        inline_rename_items(&mut moved, uid, &mut seq, &mut scope);
    }
    // asm 只立即数操作数的实参代回统一由 `fold_asm_immediate_consts` 在本轮内联全部
    // 做完后跑一遍（形参临时量就是这里生成的 `VarDecl`），不开两处口径。
    let mut items: Vec<BlockItem> = bind;
    items.extend(moved);
    // 目的地语句在出口标签**之后**：它们必须走所有路径都执行一次，而 callee 体内的
    // `goto`（展开时已加前缀）不能被它们跳过。
    // 结果临时量的**唯一个性**：它的声明类型就是 `f.ret_ty`（上面 `plain_var_decl` 用的就是
    // 它），所以读它必须按 `f.ret_ty` 读。以前 `Dest::Cond` 硬贴 `Type::Int`、`Dest::Assign`
    // 贴左值的类型，于是
    //   * `static double dsel(long);` + `if (dsel(k))` 在 -O2 按 32 位整数判真，而槽里躺的是
    //     `1.0` 的位模式（低 32 位为 0）⇒ 真被判成假（实测 gcc/dcc -O0 都返 1，-O2 返 2）；
    //   * `int i; i = dsel(k);` 按 `int` 读 ⇒ 发射层那次 `cvttsd2si` 根本没地方发生，
    //     double 的位模式被当 int 存（实测 -O2 得 0，-O0 与 gcc 得 3）。
    let rv_ty = f.ret_ty.clone();
    let ident_of = |name: String, ty: Type| Expr::Ident {
        name: Ident {
            name,
            span: rspan.clone(),
        },
        span: rspan.clone(),
        ty,
    };
    let rv_expr = || ident_of(rv_name.clone(), rv_ty.clone());
    // 把「已算好的值」写进一个左值：目的地按左值类型隐式转换。未展开前那是
    // `l = f(...)`，赋值臂会自己补那次转换；现在右值是一个带自己类型的名字，
    // 而本 pass 跑在语义分析之后没人再补，所以上面的转换必须留在 AST 上
    // （类型相同时不包，免得多一层无意义的节点）。
    let assign_from = |l: Expr, src: Expr| -> Expr {
        let lty = l.ty();
        let sty = src.ty();
        let r = if lty == sty {
            Box::new(src)
        } else {
            Box::new(Expr::Cast {
                ty: lty.clone(),
                e: Box::new(src),
                span: rspan.clone(),
            })
        };
        Expr::Assign {
            op: BinOp::Add,
            l: Box::new(l),
            r,
            span: rspan.clone(),
            ty: lty,
        }
    };
    let assign_rv = |l: Expr| -> Expr { assign_from(l, rv_expr()) };
    match dest {
        Dest::Assign(l) => {
            items.push(BlockItem::Stmt(Stmt::Expr {
                e: assign_rv(l),
                span: rspan.clone(),
            }));
        },
        Dest::AssignChain { inner, outer } => {
            // C 语义：`y = x = f()` 里内层先写，而**外层拿到的是内层转换后的值**：
            // `double y; int x; y = x = f();` 且 `f()` 返 3.5 时 `x` 是 3、`y` 也是 3.0
            // 而不是 3.5。所以两边同型（Lua 的 `L->ci = ci = prepCallInfo(...)`：三个
            // 类型同一个指针）时直读结果临时量就行；不同型时必须先落一个内层类型的
            // 中间量，外层从它读。不能「重读一次内层左值」：`a[i++] = f()` 那种下标
            // 带副作用，读两次就是推进两次。
            let ity = inner.ty();
            if ity == rv_ty {
                items.push(BlockItem::Stmt(Stmt::Expr {
                    e: assign_rv(inner),
                    span: rspan.clone(),
                }));
                items.push(BlockItem::Stmt(Stmt::Expr {
                    e: assign_rv(outer),
                    span: rspan.clone(),
                }));
            } else {
                let mid_name = format!("__inl{}$rv2", uid);
                items.push(BlockItem::Decl(Decl::Var(plain_var_decl(
                    ity.clone(),
                    mid_name.clone(),
                    None,
                    &rspan,
                ))));
                items.push(BlockItem::Stmt(Stmt::Expr {
                    e: assign_from(ident_of(mid_name.clone(), ity.clone()), rv_expr()),
                    span: rspan.clone(),
                }));
                let mid = || ident_of(mid_name.clone(), ity.clone());
                items.push(BlockItem::Stmt(Stmt::Expr {
                    e: assign_from(inner, mid()),
                    span: rspan.clone(),
                }));
                items.push(BlockItem::Stmt(Stmt::Expr {
                    e: assign_from(outer, mid()),
                    span: rspan.clone(),
                }));
            }
        },
        Dest::Cond { then_, else_ } => {
            items.push(BlockItem::Stmt(Stmt::If {
                cond: rv_expr(),
                then_: Box::new(then_),
                else_: else_.map(Box::new),
                span: rspan.clone(),
            }));
        },
        Dest::CompoundAssign {
            target,
            op,
            call_on_left,
            lvalue_read,
            binop_ty,
            assign_ty,
        } => {
            // 只把那一侧的 Call 换成结果临时量的读；左值读与两层的类型都由匹配时抄回来，
            // 所以这棵子树与未展开时除「少一个调用」外逐字相同。
            let mk_binop = |l: Expr, r: Expr| Expr::Binop {
                op,
                l: Box::new(l),
                r: Box::new(r),
                span: rspan.clone(),
                ty: binop_ty.clone(),
            };
            let rhs = if call_on_left {
                mk_binop(rv_expr(), lvalue_read)
            } else {
                mk_binop(lvalue_read, rv_expr())
            };
            items.push(BlockItem::Stmt(Stmt::Expr {
                e: Expr::Assign {
                    op: BinOp::Add,
                    l: Box::new(target),
                    r: Box::new(rhs),
                    span: rspan.clone(),
                    ty: assign_ty,
                },
                span: rspan.clone(),
            }));
        },
        // `return f(...);` 且 callee 返回 **void**：callee 的体里没有 `return`（走完体
        // 就是返回），上面那条「不改写 callee 的 return」对它无对象 —— 整条调用点的
        // `return` 只能在这里补。丢掉它，`if (x) return f();` 就变成 `if (x) { f(); }`，
        // 接着执行后面的语句。
        //
        // 实测（kbuild22 的 `--boot`）：`lib/maple_tree.c::mas_topiary_replace` 的
        // `if (mte_is_leaf(old_enode)) return mas_free(mas, old_enode);`（`mas_free` 是
        // `static inline void`）被编成「释放旧叶之后落入第二循环」，于是拿已死的旧根叶
        // 进 `mas_find_child`：叶的 `ma_data_end` 返回 15 ⇒ 读 `slots[15]`（leaf 的 meta
        // 区，恒 NULL）⇒ `mte_parent(NULL)` 解引用地址 0，early_irq_init 期间 panic。
        // 非 void 的 callee 不走这里：它的每条路径都自带 `return`（落空是 UB），
        // 那些 `return` 原样搬过来就是本层的 `return`。
        Dest::ReturnValue => {
            if matches!(rv_ty, Type::Void) {
                items.push(BlockItem::Stmt(Stmt::Return {
                    val: None,
                    span: rspan.clone(),
                }));
            }
        },
        Dest::Discard => {},
    }
    // 搬进来的体先做一次「形参临时量 → caller 实参」的代回，再折掉条件已是编译期值的分支。
    // 必须在递归展开之前做完（理由见 `prune_spliced_items`）。
    {
        prune_spliced_items(&mut items);
    }
    *s = Stmt::Block {
        b: Block {
            items,
            span: rspan.clone(),
        },
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
    level: u8,
    caller_flatten: bool,
    depth: u32,
    walk_arms: bool,
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
    // 报障名单：点名的 callee 一律不展开（见 [`inline_not_listed`]）。
    if inline_not_listed(&callee_name) {
        trace_inline_decision(&callee_name, "REJECT", "DCC_INLINE_NOT 点名否决");
        return false;
    }
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
    if depth >= INLINE_MAX_DEPTH_FORCED {
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
    // 空体不能走语句展开：它既不是表达式体也没东西可搬。
    if body.items.is_empty() {
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
            // 不带 goto 标签的内联汇编语句：它就是一条完整、无控制流的语句，展开
            // 只是原样搬进 caller 的块里（操作数改名由 `rename_idents_in_stmt` 负责）。
            // 不接它就没有内联：内核 `__always_inline` 的 `alternative_msr_write`（
            // arch/x86/include/asm/nospec-branch.h）与 `_static_cpu_has`（cpufeature.h）
            // 体就是「asm 语句 + return」，它们的 `[feature] "i" (bit)` 只在内联后才能
            // 退化成立即数；不内联时只能拿到寄存器，`.4byte %c[feature]` 就变成
            // 一条指向寄存器名的重定位。
            // `asm goto` 不接：它的控制流目标在 callee 的作用域里，搬进 caller 需要
            // 一起搬标签，超出「只接直线性体」的不变式。
            BlockItem::Stmt(st @ Stmt::AsmGoto { goto_labels, .. }) => {
                if !goto_labels.is_empty() {
                    return false;
                }
                mid.push(st.clone());
            },
            BlockItem::Stmt(Stmt::Return { val, .. }) if i + 1 == body.items.len() => {
                ret_val = Some(val.clone());
            },
            // 非 return 结尾/中间 return/其他语句形态: 不展开
            _ => return false,
        }
    }
    // 结尾 return 可省：无返回值（`void`）的 `__always_inline` 内核分派在今天的写法
    // 就是「只有 asm 语句」的体（`arch/x86/include/asm/nospec-branch.h` 的
    // `alternative_msr_write`）。旧规则在这里直接 `return false`，于是不内联，它那个
    // `[feature] "i" (feature)` 只能拿到寄存器，`.4byte %c[feature]` 就变成一条指向
    // 寄存器名的野重定位。只有「调用点是表达式语句」时才能省 return（结果被丢），
    // `return f(...)` 的调用点仍然要求 callee 自己给出返回值。
    let ret_val = match ret_val {
        Some(v) => v,
        None => {
            // `Type::Void` 就是无返回值的判据（`ast::Type` 没提供 `is_void`）。
            if in_return || !matches!(f.ret_ty, crate::ast::Type::Void) {
                trace_inline_decision(&callee_name, "REJECT", "stmt-inline: no trailing return");
                return false;
            }
            None
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
    // 与通用那条臂同一手法的趁早剪枝：形参临时量先代回 caller 实参，再折掉条件已是
    // 编译期值的分支，然后才让递归去展开剩下的调用点（理由见 `try_static_inline_general`
    // 里那段注释：晚一步就只能面对一个带 `Stmt::Label`、再也剪不动的死块）。
    {
        prune_spliced_items(&mut items);
    }
    *s = Stmt::Block {
        b: Block {
            items,
            span: rspan.clone(),
        },
        span: rspan,
    };
    // 展开块内仍可能有嵌套调用 (如 callee 调用另一个 always_inline)，**按 caller 的
    // 档位继续**代 —— 这里以前写死传 `0`，后果是展开块里那层递归只能用「语句位置的
    // 强制内联」一条通路：`inline_forced_call_as_stmt_expr` 的准入是 `level >= 2`（它要
    // 造语句表达式），于是嵌在条件/比较/实参里的 `__always_inline` 调用留在原地，
    // 逼编译器另发一份外带副本 ⇒ 副本里 `switch (形参) { default: BUILD_BUG(); }` 的
    // 全部引用都在（实测 `mm/memory.o`：`copy_p4d_range` 展开到第 7 层时
    // `__folio_rmap_sanity_checks` 的调用停在 level=0 这道闸上，产物里留 2 条
    // `__compiletime_assert_466/467`，而 gcc 侧这两个名字连定义都不存在）。
    if let Stmt::Block { b, .. } = s {
        // 预算沿用外层 caller 的判决，不能在这里重算：展开块只包含 callee 的体（它必然比
        // 原 caller 小），每次展开都重新拿一份预算等于给超上限的 caller 开了一个绕过口。
        // 今天这条路径只接平坦体（结构检查在上面），臂体走不到，但下一步放宽平坦限制时
        // 它就会成立，所以现在就把不变式钉住。
        inline_calls_in_block(b, funcs, level, caller_flatten, false, false, depth + 1, walk_arms);
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

/// 形参替换的一次性上下文。除遮蔽栈外还带一对「**当前是否在 `&` 下面**」的状态，
/// 因为「形参被取址」是表达式内联的**准入判据**，而不是事后能补的东西：
/// 体里的 `&p` 如果把 `p` 直接换成调用者的表达式，`&p` 就变成了「调用者那个对象的地
/// 址」—— 被调方通过它写回时就直接改到了调用者的变量。
/// 现场（`scripts/kconfig/preprocess.c`，-O2 才发）：`expand_string_with_args(const char *in)
/// { return __expand_string(&in, ...); }` 被表达式内联进 `variable_expand` 后，第一个实参
/// 变成 `AddrOf{ Member{ v, field: "value" } }`，`__expand_string` 里那句 `*str = p`
/// 于是把 `v->value` 改写成了循环结束指针（+10），后面 `variable_all_del()` 去 free 它就是
/// `free(): invalid pointer`（内核侧症状：`make syncconfig` 崩 + 刷 315 条 Kconfig 错）。
/// 语句级内联没这个问题：它本来就把每个形参存进一份额外的局部（`__inlN$_p`）。
struct SubstCtx {
    /// 块作用域遮蔽栈（内层同名声明不得被当成形参换掉）。
    shadows: Vec<Vec<String>>,
    /// 正在 `&` 的操作数子树里。
    in_addr: bool,
    /// 正在 `asm` 操作数的位置：输出操作数（`"=r"(p)`/`"+m"(p)`）会被汇编**写**，
    /// 内存约束的输入（`m`/`g`/`o`/`V`/`X`）取的是形参地址 —— 与 `in_addr` 同一类
    /// 问题：换成实参表达式后，被调方写回的就是调用者的对象。
    in_asm_write: bool,
    /// 已发现「被取址的位置上出现了形参」⇒ 调用方必须放弃这次内联。
    needs_slot: bool,
}

/// 形参绑定替换 (内联展开的核心)。`pack_args` 非空时, 表达式里出现的
/// `Expr::VaArgPack` 展开为这串实参 (原地 splicing 进所在 Call 的实参表)
/// —— `__builtin_va_arg_pack ()` 的内联语义, 见 `try_inline_variadic_pack`。
/// 返回 `true` = 体里有形参被取址，表达式内联给不了它独立槽位，调用方**不得**采用结果。
fn substitute_params(expr: Expr, params: &[(String, Expr)], pack_args: &[Expr]) -> (Expr, bool) {
    let mut e = expr;
    let mut ctx = SubstCtx {
        shadows: Vec::new(),
        in_addr: false,
        in_asm_write: false,
        needs_slot: false,
    };
    subst_params_mut(&mut e, params, pack_args, &mut ctx);
    (e, ctx.needs_slot)
}

/// `substitute_params` 的本体：**穷举到每一条含子表达式的臂**。
///
/// 旧实现只下探 Ident/Binop/Unary/Cast/Cond/Comma/Call，其余一律 `other => other`，
/// 于是「形参出现在 member 链上」时它原地留了原名。实证（`fs/splice.o` 构建失败）：
///
/// ```c
/// static inline bool pipe_buf_get(struct pipe_inode_info *pipe, struct pipe_buffer *buf)
/// { return buf->ops->get(pipe, buf); }
/// ```
///
/// 体是 `Call{ callee: Member{ Member{ Ident(buf), ops }, get }, args: [pipe, buf] }`。
/// `Call` 臂存在、`Member` 臂不存在 ⇒ callee 侧那个 `buf` 原样搬进 caller；实参侧
/// `(pipe, buf)` 反倒替换了，所以只有 member 链上那一个漏。展开后 `do_splice`/`do_tee`
/// 体里凭空多出一个从未声明过的 `buf`：`plan_late_slot_budget` 当然也没给过它槽，
/// 发射期 `var_offset` 撞 E1134 + E1133（帧规划是构建期硬判据，整个 TU 编译失败）。
///
/// 形参的替换面必须与「表达式能写在哪」同宽：member/下标/取址/解引用/前后缀自增/
/// `sizeof`/复合字面量/asm 操作数/`offsetof` 数组下标/`va_arg` —— 少一条臂就是一个
/// 能在 caller 里活下来的幽灵名字。
///
/// `shadows` 是块作用域栈（现在挂在 [`SubstCtx`] 上）：内层同名声明遮蔽形参时**必须停下**，否则会把内层对象
/// 读成调用点的实参。语句表达式 `({ … })` 与每个块各自压一层。
fn subst_params_mut(
    e: &mut Expr,
    params: &[(String, Expr)],
    pack_args: &[Expr],
    ctx: &mut SubstCtx,
) {
    if let Expr::Ident { name, .. } = e {
        let nm = name.name.clone();
        if !ctx.shadows.iter().any(|s| s.iter().any(|n| *n == nm)) {
            if let Some((_, arg)) = params.iter().find(|(n, _)| *n == nm) {
                if ctx.in_addr || ctx.in_asm_write {
                    // `&形参` 与 `asm` 会写/取址的操作数 —— 换成实参表达式会把「被调方的
                    // 局部」变成「调用者对象的地址」，被调方写回就是越界改人。
                    // 见 [`SubstCtx`]。
                    ctx.needs_slot = true;
                }
                *e = arg.clone();
            }
        }
        return;
    }
    match e {
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            subst_params_mut(l, params, pack_args, ctx);
            subst_params_mut(r, params, pack_args, ctx);
        },
        // `&` 的操作数整棵都在取址语境里：进去前改标记、出来还原（见 [`SubstCtx`]）。
        Expr::AddrOf { e: x, .. } => {
            let outer = ctx.in_addr;
            ctx.in_addr = true;
            subst_params_mut(x, params, pack_args, ctx);
            ctx.in_addr = outer;
        },
        Expr::Unary { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. }
        | Expr::VaArg { ap: x, .. } => subst_params_mut(x, params, pack_args, ctx),
        // `sizeof`/`_Alignof` 的操作数是**类型语境**，不求值：形参在那个位置读的是
        // 自己**衰减后的类型**（`int a[3]` 形参是 `int *`），换成调用点的实参表达式就是
        // 换了类型。实测（`tests/fixtures/c/decl_param_decay.c`，-O2 bad=11）：
        //   内联后 `sizeof(a)` 变成 `sizeof(m[0])`（int[4]）⇒ 16 而 gcc 是 8，
        //   bp1/bp2/bp4 三族全错。这两形保持原样（旧实现本来就如此），换来的代价是
        //   「未折叠的 sizeof(形参)」会在展开体里留个名字 —— 但 sizeof 在语义分析阶段
        //   就是常量，发射期不会读到它。
        Expr::SizeofExpr { .. } | Expr::AlignofExpr { .. } => {},
        Expr::Member { expr: x, .. } => subst_params_mut(x, params, pack_args, ctx),
        Expr::Index { arr, idx, .. } => {
            subst_params_mut(arr, params, pack_args, ctx);
            // **下标不是被取址的对象**：`&arr[i]` 里 `i` 只贡献那个值，替换成实参
            // 与 `&arr[3]` 是同一件事，被调方拿到的仍是 `arr` 里某元素的地址。
            // 漏了这条，`in_addr` 一路带进下标，`static_key_false(&perf_swevent_enabled[swevt])`
            // 这种体就被判成「形参被取址」而拒绝表达式内联，`__always_inline` 于是留一份
            // 外联副本 —— 副本里 `swevt` 是运行期形参，`"i"(&arr[swevt])` 编不出来：
            // 实测 `kernel/sched/core.c` 报
            // `E0311 ... (expression: __inl9098$_key, in function __perf_sw_enabled)`，
            // 而最小复现（三层 always_inline + 一层 static inline）同样只在这一处拒。
            let outer = ctx.in_addr;
            ctx.in_addr = false;
            subst_params_mut(idx, params, pack_args, ctx);
            ctx.in_addr = outer;
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            ..
        } => {
            subst_params_mut(cond, params, pack_args, ctx);
            subst_params_mut(then_, params, pack_args, ctx);
            subst_params_mut(else_, params, pack_args, ctx);
        },
        Expr::Call { callee, args, .. } => {
            subst_params_mut(callee, params, pack_args, ctx);
            // `f(a, __builtin_va_arg_pack())` → `f(a, pack0, pack1, ...)`:
            // pack 占一个实参位, 展开成调用方多余实参的**整串**。
            if !pack_args.is_empty() && args.iter().any(|a| matches!(a, Expr::VaArgPack { .. })) {
                let mut out: Vec<Expr> = Vec::with_capacity(args.len() + pack_args.len());
                for mut a in std::mem::take(args) {
                    match a {
                        Expr::VaArgPack { .. } => out.extend(pack_args.iter().cloned()),
                        _ => {
                            subst_params_mut(&mut a, params, pack_args, ctx);
                            out.push(a);
                        },
                    }
                }
                *args = out;
            } else {
                for a in args.iter_mut() {
                    subst_params_mut(a, params, pack_args, ctx);
                }
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits.iter_mut() {
                match it {
                    InitItem::Expr(x) => subst_params_mut(x, params, pack_args, ctx),
                    InitItem::Designated { value, .. } => {
                        subst_params_mut(value, params, pack_args, ctx)
                    },
                }
            }
        },
        Expr::AsmExpr {
            inputs,
            outputs,
            input_constraints,
            ..
        } => {
            // 操作数位置：输出操作数恒写；输入操作数只有允许内存形式时才算取址。
            // 两种都要让形参保留一份独立副本（[`SubstCtx::in_asm_write`]）。
            let outer = ctx.in_asm_write;
            for x in outputs.iter_mut() {
                ctx.in_asm_write = true;
                subst_params_mut(x, params, pack_args, ctx);
            }
            for (i, x) in inputs.iter_mut().enumerate() {
                ctx.in_asm_write = outer
                    || input_constraints
                        .get(i)
                        .is_some_and(|c| asm_constraint_takes_address(c));
                subst_params_mut(x, params, pack_args, ctx);
            }
            ctx.in_asm_write = outer;
        },
        Expr::Offsetof { indices, .. } => {
            for x in indices.iter_mut() {
                subst_params_mut(x, params, pack_args, ctx);
            }
        },
        // 语句表达式自成一个块作用域：块内声明遮蔽形参，出去时弹栈。
        Expr::StmtExpr { block, .. } => {
            let mut names: Vec<String> = Vec::new();
            collect_block_decl_names(&block.items, &mut names);
            ctx.shadows.push(names);
            subst_params_block_items(&mut block.items, params, pack_args, ctx);
            ctx.shadows.pop();
        },
        // 字面量 / `sizeof(type)` / `alignof(type)` / `&&label` / `va_arg_pack`：
        // 不含可替换的对象名。穷举到这里，新增变体会在本函数编不过。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::LabelAddr { .. }
        | Expr::VaArgPack { .. } => {},
        Expr::Ident { .. } => {},
    }
}

/// 本层块作用域的声明名（不含嵌套块：那些块自己压栈，见 `subst_params_stmt`）。
fn collect_block_decl_names(items: &[BlockItem], out: &mut Vec<String>) {
    for it in items {
        if let BlockItem::Decl(Decl::Var(v)) = it {
            out.push(v.name.name.clone());
        }
    }
}

fn subst_params_block_items(
    items: &mut [BlockItem],
    params: &[(String, Expr)],
    pack_args: &[Expr],
    ctx: &mut SubstCtx,
) {
    for it in items.iter_mut() {
        match it {
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(e) = &mut v.init {
                    subst_params_mut(e, params, pack_args, ctx);
                }
            },
            BlockItem::Decl(_) => {},
            BlockItem::Stmt(s) => subst_params_stmt(s, params, pack_args, ctx),
        }
    }
}

fn subst_params_stmt(
    s: &mut Stmt,
    params: &[(String, Expr)],
    pack_args: &[Expr],
    ctx: &mut SubstCtx,
) {
    match s {
        Stmt::Expr { e, .. } => subst_params_mut(e, params, pack_args, ctx),
        Stmt::Return { val: Some(e), .. } => subst_params_mut(e, params, pack_args, ctx),
        Stmt::Return { val: None, .. } => {},
        Stmt::If {
            cond,
            then_,
            else_,
            ..
        } => {
            subst_params_mut(cond, params, pack_args, ctx);
            subst_params_stmt(then_, params, pack_args, ctx);
            if let Some(el) = else_ {
                subst_params_stmt(el, params, pack_args, ctx);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            subst_params_mut(cond, params, pack_args, ctx);
            subst_params_stmt(body, params, pack_args, ctx);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                subst_params_stmt(i, params, pack_args, ctx);
            }
            if let Some(c) = cond {
                subst_params_mut(c, params, pack_args, ctx);
            }
            if let Some(st) = step {
                subst_params_mut(st, params, pack_args, ctx);
            }
            subst_params_stmt(body, params, pack_args, ctx);
        },
        // 块自己的作用域：块内顶层声明遮蔽形参，出去时弹栈。
        Stmt::Block { b, .. } => {
            let mut names: Vec<String> = Vec::new();
            collect_block_decl_names(&b.items, &mut names);
            ctx.shadows.push(names);
            subst_params_block_items(&mut b.items, params, pack_args, ctx);
            ctx.shadows.pop();
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            subst_params_mut(expr, params, pack_args, ctx);
            // `switch` 体是一整个块作用域：prelude 与各 case 体的顶层声明都算本层。
            let mut names: Vec<String> = Vec::new();
            collect_block_decl_names(prelude, &mut names);
            for c in cases.iter() {
                collect_block_decl_names(&c.body.items, &mut names);
            }
            ctx.shadows.push(names);
            subst_params_block_items(prelude, params, pack_args, ctx);
            for c in cases.iter_mut() {
                if let Some(v) = &mut c.val {
                    subst_params_mut(v, params, pack_args, ctx);
                }
                if let Some(v) = &mut c.range_end {
                    subst_params_mut(v, params, pack_args, ctx);
                }
                subst_params_block_items(&mut c.body.items, params, pack_args, ctx);
            }
            ctx.shadows.pop();
        },
        Stmt::Label { stmt, .. } => subst_params_stmt(stmt, params, pack_args, ctx),
        Stmt::GotoIndirect { expr, .. } => subst_params_mut(expr, params, pack_args, ctx),
        // `break`/`continue`/`goto`/`fallthrough`/`asm goto`/EH 落地垫：单 return 体的
        // 内联通道本来就不接这些形状（`expr_jumps_out` 挡着），这里保持原样。
        _ => {},
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
    // 报障名单：点名的 callee 一律不展开（见 [`inline_not_listed`]）。
    if inline_not_listed(name) {
        trace_inline_decision(name, "REJECT", "DCC_INLINE_NOT 点名否决");
        return None;
    }
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
            // 同下：只搬表达式，标签/跳转留在 callee 体里就是悬空引用。
            if expr_jumps_out(ret) {
                trace_inline_decision(name, "REJECT", "forced 表达式体里带标签/跳转");
                return None;
            }
            trace_inline_decision(name, "INLINE", "always_inline (expression body)");
            return substitute_call(f, ret, args, name);
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

    // --- -O2 起: 小型 `static` 单表达式函数的逐表达式替换 ---
    //
    // 以前这一档要求 `level >= 3` 且形参为空。后果不是「少一个优化」而是**产物多一份
    // 外带副本**：`static inline` 的调用点只要落在**非语句位置**（`return f(x) * 2`、
    // `a + f(b)`、实参位置），三条语句级通道（`try_stmt_inline_call` /
    // `try_static_inline_general` / `inline_forced_call_as_stmt_expr` 的非强制分支）
    // 一条都接不到它，于是定义必须留一份独立副本。内核里这批副本正是「陷阱函数引用」
    // 的主产地：`static inline void __folio_remove_rmap(…, enum pgtable_level level)`
    // 的外带副本里 `switch (level)` 的选择子是**运行时形参**，`default: BUILD_BUG()`
    // 于是永远可达，产物里留下 `__compiletime_assert_466` 的调用 —— gcc 不发射这份
    // 副本（它的 `-O2 -finline-functions` 对内部链接小函数**没有语句位置限制**），
    // 所以全树任何 .o 里都不存在这个名字（`nm` 实测：`mm/rmap.o` dcc 4 个 W、gcc 0 个）。
    //
    // 放宽的安全前提是**实参无副作用**：逐表达式替换是「把实参按形参出现次数克隆进
    // 返回表达式」，有副作用的实参会被求值多次（`f(g())` 体里用两次 p ⇒ g() 跑两次）。
    // 这条不是保守估计而是正确性判据，所以与成本模型并列、不看档位。
    let cost = estimate_inline_cost(f, body, args);
    let profile_relaxed = caller_is_hot && level >= 2;
    let pure_args = args
        .iter()
        .all(|a| !crate::semantics::expr_has_side_effects(a));
    let heuristic_ok = if profile_relaxed {
        cost <= INLINE_COST_THRESHOLD * 2
    } else {
        f.is_static
            && level >= 2
            && pure_args
            && cost <= INLINE_COST_THRESHOLD
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
    // 表达式级替换只搬**返回表达式**，callee 体里的标签定义留在原地 ⇒ 体里带
    // `&&label` / `goto` 的搬过去就是悬空标签。这一条不看档位：无论 -O2 的静态小函数
    // 还是 always_inline，搬移前的形状判据一样。
    if expr_jumps_out(ret) {
        trace_inline_decision(name, "REJECT", "表达式体里带标签/跳转 (不可只搬表达式)");
        return None;
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
    substitute_call(f, ret, args, name)
}

/// 形参 ← 实参绑定：每个实参先按**形参声明类型**做一次隐式转换 (C11 6.5.2.2p7)，
/// 再交给 [`substitute_params`] 做名字替换。
///
/// 少了这次转换，实参类型会「泄漏」给形参：
/// ```c
/// static inline struct request *blk_mq_rq_from_pdu(void *pdu)
/// { return pdu - sizeof(struct request); }
/// static inline struct request *scsi_cmd_to_rq(struct scsi_cmnd *cmd)
/// { return blk_mq_rq_from_pdu(cmd); }
/// ```
/// 内联后 `pdu - 0xf8` 里的 `pdu` 拿到的是实参的 `struct scsi_cmnd *`，
/// 指针算术按 `sizeof(struct scsi_cmnd)=0x128` 缩放 ⇒ `cmd - 0x11ec0`（gcc 是
/// `cmd - 0xf8`）。`__ata_scsi_queuecmd` 由此读到垃圾 `rq->tag`，`__ata_qc_from_tag()`
/// 返回 NULL，随后 `qc->tag = qc->hw_tag = tag` 写 0x5c 触发内核 boot Oops。
///
/// 语句级内联 (`try_stmt_inline_call`) 建的是带 `p.ty` 的临时量，天然没有这个问题；
/// 只有「把返回表达式整体搬进调用方」的表单级内联需要在这里补转换。
fn bind_inline_params(f: &FuncDecl, args: &[Expr]) -> Vec<(String, Expr)> {
    f.params
        .iter()
        .zip(args.iter())
        .filter_map(|(p, a)| {
            let id = p.name.as_ref()?;
            Some((
                id.name.clone(),
                cast_to_declared_ty(a.clone(), &p.ty, p.span.clone()),
            ))
        })
        .collect()
}

/// 实参 → 形参声明类型 / 返回表达式 → 函数声明返回类型的隐式转换 (C11 6.5.2.2p7、
/// 6.8.6.4p3)；类型已相同则原样返回（不给每个内联点都套 cast）。
fn cast_to_declared_ty(a: Expr, ty: &Type, span: Span) -> Expr {
    if crate::ast::type_equal(&a.ty(), ty) {
        return a;
    }
    // 不能写 cast 的类型（聚合/向量/`Type::Func`/未解析的 `Named`）原样返回：
    // 聚合按值传递时类型本就相容，`Type::Func` 只会作为函数指示符实参出现，
    // 其形参类型是 `Ptr(Func(..))`，走指针分支。
    let writable = matches!(
        ty,
        Type::Ptr(_)
            | Type::Bool
            | Type::Char
            | Type::SChar
            | Type::UChar
            | Type::Char8
            | Type::Char16
            | Type::Char32
            | Type::Short
            | Type::UShort
            | Type::Int
            | Type::UInt
            | Type::Long
            | Type::ULong
            | Type::LongLong
            | Type::ULongLong
            | Type::Int128
            | Type::UInt128
            | Type::Float
            | Type::Double
            | Type::LongDouble
            | Type::Float128
            | Type::Fract
            | Type::Accum
            | Type::Enum(..)
    );
    if !writable {
        return a;
    }
    Expr::Cast {
        ty: ty.clone(),
        e: Box::new(a),
        span,
    }
}

/// 参数替换: 将函数体返回表达式中的形参替换为实参.
/// 返回 `None` = 这个体不能只搬表达式（有形参被取址，见 [`SubstCtx`]）。
fn substitute_call(f: &FuncDecl, ret: &Expr, args: &[Expr], name: &str) -> Option<Expr> {
    let bindings = bind_inline_params(f, args);
    if bindings.len() != f.params.len() {
        return Some(ret.clone());
    }
    let (e, needs_slot) = substitute_params(ret.clone(), &bindings, &[]);
    if needs_slot {
        trace_inline_decision(name, "REJECT", "形参被取址 (&p)，表达式内联给不了独立槽位");
        return None;
    }
    // 返回表达式也要回到**声明返回类型**：C11 6.8.6.4p3。只搬表达式时类型会跟着
    // 实参走，实测 `static inline struct request *scsi_cmd_to_rq(struct scsi_cmnd *cmd)
    // { return blk_mq_rq_from_pdu(cmd); }` 内联后整个调用点成了 `void *`，于是
    // `scsi_cmd_to_rq(cmd)->tag` 的成员偏移解析不到，按 0 发射 —— 把 `struct request`
    // 头 4 字节（`list_head.next` 低半）当成 tag，`__ata_scsi_queuecmd` 又写 0x5c。
    Some(cast_to_declared_ty(e, &f.ret_ty, f.span.clone()))
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
    let bindings = bind_inline_params(f, args);
    if bindings.len() != f.params.len() {
        return None; // 无形名实参 (如 `void f(int, ...)`): 无法绑定
    }
    let pack_args = &args[f.params.len()..];
    trace_inline_decision(name, "INLINE", "always_inline variadic + va_arg_pack");
    let (e, needs_slot) = substitute_params(ret, &bindings, pack_args);
    if needs_slot {
        trace_inline_decision(name, "REJECT", "形参被取址 (&p)，表达式内联给不了独立槽位");
        return None;
    }
    Some(cast_to_declared_ty(e, &f.ret_ty, f.span.clone()))
}

/// 取 always_inline 函数体的返回值表达式。除单 `Return` 外, 还认
/// `[If{c, then: Return(a), else: None}, Return(b)]` (≡ `return c ? a : b;`)
/// —— 变参 va_arg_pack 转发函数的典型形状 (va-arg-pack-1 的 bar)。
fn inline_body_return_expr(body: &Block) -> Option<Expr> {
    if let [BlockItem::Stmt(Stmt::Return { val: Some(ret), .. })] = body.items.as_slice() {
        return Some(ret.clone());
    }
    if let [BlockItem::Stmt(Stmt::If {
        cond,
        then_,
        else_: None,
        span,
    }), BlockItem::Stmt(Stmt::Return {
        val: Some(ret2), ..
    })] = body.items.as_slice()
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
        Expr::Binop { l, r, .. } => expr_contains_va_arg_pack(l) || expr_contains_va_arg_pack(r),
        Expr::Unary { e, .. } | Expr::Cast { e, .. } => expr_contains_va_arg_pack(e),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_contains_va_arg_pack(cond)
                || expr_contains_va_arg_pack(then_)
                || expr_contains_va_arg_pack(else_)
        },
        Expr::Comma { l, r, .. } => expr_contains_va_arg_pack(l) || expr_contains_va_arg_pack(r),
        Expr::Assign { l, r, .. } => expr_contains_va_arg_pack(l) || expr_contains_va_arg_pack(r),
        _ => false,
    }
}

/// 表达式位置上的一个调用点能不能就地展开：先试「单 return 体」逐表达式替换，
/// 不行再试「强制内联 + 语句表达式包装」。两条的准入与原来逐字相同，拆成函数
/// 只是为了让 [`inline_expr`] 能在「实参下探」**前后各跑一次**（见那边注释）。
#[allow(clippy::too_many_arguments)]
fn try_inline_call_at_expr_pos(
    callee: &Expr,
    args: &[Expr],
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
    call_ty: &Type,
    call_span: &Span,
    allow_effectful_args: bool,
) -> Option<Expr> {
    let name = match callee {
        Expr::Ident { name, .. } => name.name.clone(),
        _ => return None,
    };
    if let Some(inlined) = try_inline_call(
        &name,
        args,
        funcs,
        level,
        caller_flatten,
        caller_is_hot,
        depth,
    ) {
        // 内联后继续递归内联替换结果中的嵌套调用
        return Some(inline_expr(
            inlined,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth + 1,
        ));
    }
    // 表达式级替换不接这一形（体不是「一条 `return 表达式;`」），而
    // `always_inline` 是语义要求而不是优化建议：GCC 无条件展开，调用点
    // 嵌在 `?:`/`&&`/实参里也一样。这里在调用**原本的求值位置**展开。
    inline_forced_call_as_stmt_expr(
        &name,
        args,
        funcs,
        level,
        caller_flatten,
        caller_is_hot,
        fenv_access,
        depth,
        call_ty.clone(),
        call_span.clone(),
        allow_effectful_args,
    )
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
            if let Some(expanded) = try_inline_call_at_expr_pos(
                &callee, &args, funcs, level, caller_flatten, caller_is_hot, fenv_access, depth,
                &ty, &span, false,
            ) {
                return expanded;
            }
            // 重试那一轮能不能放宽「实参有副作用」这条否决，只看下探前实参里有没有
            // 「必须展开」的调用点：有就是自己刚把那个调用展开成语句表达式了（副作用
            // 就是那次展开），与「实参里原有一个不可内联的函数调用」不是一回事。
            let args_have_forced = args
                .iter()
                .any(|a| expr_has_forced_inline_call(a, funcs, caller_flatten));
            // 下探前的副作用快照。重试的条件不是「实参里有没有 forced 调用」，而是
            // 「实参里有副作用」：`inline_forced_call_as_stmt_expr` 借道语句级通用臂，
            // 形参临时量只对实参求值一次且固定从左到右（见 `try_static_inline_general`
            // 的 `allow_effectful_args` 注释），与直接调用的求值次数逐字相同，所以实参
            // 里原本就有的普通调用（如 `se_runnable(se)`）不应再否决展开。内核
            // `if (___update_load_sum(now, &se->avg, !!se->on_rq, se_runnable(se), …))`
            // 正是这一形状：`se_runnable` 是普通 static inline（体含 if，表达式位置
            // 不展开），于是外层 `__always_inline` 被“实参有副作用”永久否决，每处
            // 留一份外带副本 + 真调用（实测调度链多一层帧，16K 内核栈在
            // kernel_init 期撞底）。非强制 callee 仍由 `inline_forced_call_as_stmt_expr`
            // 内部的 `forced` 闸门拦着，本重试不会把普通 static 迁进表达式位置。
            let args_dirty_before = args
                .iter()
                .any(crate::semantics::expr_has_side_effects);
            let callee_boxed = Box::new(inline_expr(
                *callee,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            ));
            let args: Vec<Expr> = args
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
                .collect();
            // 实参下探完再试一次外层：`f(g(...))` 里 `g` 的展开把实参换成了语句表达式，
            // 而 `inline_forced_call_as_stmt_expr` 的「实参有副作用就不展开」在第一轮上
            // 正是否决这条的判据（它防的是「实参里嵌着一个要递归多一层同步的可内联调用」）。
            // 那一层递归已经在下面走完了，剩下的就是一棵已展开的树，外层可以安心搬进
            // 形参临时量。展开顺序因此是由内向外，与 GCC 对 `__always_inline` 的做法一致。
            let args_dirty_after = args
                .iter()
                .any(crate::semantics::expr_has_side_effects);
            if args_have_forced || args_dirty_before || args_dirty_after {
                if let Some(expanded) = try_inline_call_at_expr_pos(
                    &callee_boxed, &args, funcs, level, caller_flatten, caller_is_hot,
                    fenv_access, depth, &ty, &span, true,
                ) {
                    return expanded;
                }
            }
            Expr::Call {
                callee: callee_boxed,
                args,
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
        // 「只包一层子表达式」的节点也要下探。`Expr::Cast` 这批以前落进下面的
        // `other => other`，于是嵌在转型里的 `__always_inline` 调用永远不会被展开：
        // 而内核恰好处处是这个写法（`arch/x86/include/asm/asm.h` 的
        // `return (unsigned long)rip_rel_ptr(&v)`、`(typeof(x))f(...)`、
        // `if ((long)static_key_false(&k))`）。不展开的代价不是「少一个优化」而是
        // **产物错**：callee 另发一份独立副本，它自己的 `"i" (形参)` 只能拿到寄存器
        // （实测报 `impossible constraint in 'asm'` 于 `arch/x86/include/asm/asm.h:121`）。
        //
        // 但 `SizeofExpr`/`AlignofExpr`/`Offsetof` **不下探**：C 里它们的操作数根本不
        // 求值，把带 asm 的体展开进去等于凭空多一次副作用（与 `discarded_value_needs_code`
        // 把它们归为常量同一理由）。
        Expr::Cast { ty, e: inner, span } => Expr::Cast {
            ty,
            e: Box::new(inline_expr(
                *inner,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            span,
        },
        Expr::AddrOf { e: inner, ty, span } => Expr::AddrOf {
            e: Box::new(inline_expr(
                *inner,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            ty,
            span,
        },
        Expr::Deref { e: inner, ty, span } => Expr::Deref {
            e: Box::new(inline_expr(
                *inner,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            ty,
            span,
        },
        Expr::Real { e: inner, ty, span } => Expr::Real {
            e: Box::new(inline_expr(
                *inner,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            ty,
            span,
        },
        Expr::Imag { e: inner, ty, span } => Expr::Imag {
            e: Box::new(inline_expr(
                *inner,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            ty,
            span,
        },
        Expr::Index { arr, idx, ty, span } => Expr::Index {
            arr: Box::new(inline_expr(
                *arr,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            idx: Box::new(inline_expr(
                *idx,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            ty,
            span,
        },
        // 【offsetof 的下标】`offsetof(struct blob, arr[i][ilog2(m)])` 里的下标是**运行时**
        // 表达式（AST 为此专门留了 `indices`），不递进去 ⇒ 那里的 `__always_inline` 调用
        // 永远轮不到展开。现场（实测 `drivers/gpu/drm/i915/gt/uc/intel_guc_ads.o` 的
        // `intel_guc_engine_usage_record_map`）：`ilog2(engine->logical_mask)` 选出的
        // `__ilog2_u32`（`include/linux/log2.h:20` 的 `static __always_inline`）留在产物里
        // 成了外部引用，而 gcc 全内联、一个引用都不发。
        Expr::Offsetof {
            ty, member, indices, span, result_ty,
        } => Expr::Offsetof {
            ty,
            member,
            indices: indices
                .into_iter()
                .map(|ix| {
                    Box::new(inline_expr(
                        *ix,
                        funcs,
                        level,
                        caller_flatten,
                        caller_is_hot,
                        fenv_access,
                        depth,
                    ))
                })
                .collect(),
            span,
            result_ty,
        },
        Expr::Member {
            expr: base,
            field,
            ty,
            is_arrow,
            span,
        } => Expr::Member {
            expr: Box::new(inline_expr(
                *base,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            )),
            field,
            ty,
            is_arrow,
            span,
        },
        Expr::Assign { op, l, r, ty, span } => Expr::Assign {
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
            ty,
            span,
        },
        Expr::Comma { l, r, ty, span } => {
            let l = inline_expr(
                *l,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            );
            // 【逗号左侧的调用：值被丢弃】`(f(...), expr)` 里 `f` 的返回值没人要，
            // 而它偏偏落在「值位置」—— `inline_expr` 只为值位置准备了「接住返回值」
            // 的两条通道，展开一个 void 的 static 小函数就更不在话下。不展开的代价
            // 不是少一个优化，是**多一份 out-of-line 副本**，而副本里的形参是运行时值：
            // `include/linux/io_uring_types.h` 的
            //     #define io_kiocb_to_cmd(req, cmd_type) (\
            //         io_kiocb_cmd_sz_check(sizeof(cmd_type)), ((cmd_type *)&(req)->cmd))
            // 让 21 个 io_uring TU 全部留下一条 `__compiletime_assert_NNN`
            // （`static inline void io_kiocb_cmd_sz_check(size_t cmd_sz) { BUILD_BUG_ON(...) }`
            // 单独发一份时 `cmd_sz > sizeof()` 永远折不掉）；gcc 那一侧整块消失。
            let l = match inline_discarded_call_expr(
                &l,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
                false,
                true,
            ) {
                Some(expanded) => expanded,
                None => l,
            };
            Expr::Comma {
                l: Box::new(l),
                r: Box::new(inline_expr(
                    *r,
                    funcs,
                    level,
                    caller_flatten,
                    caller_is_hot,
                    fenv_access,
                    depth,
                )),
                ty,
                span,
            }
        },
        // 语句表达式 `({...})` 的块体里同样有调用点：内核的 `WARN*()`、`__get_user_x()`
        // 这一族宏把求值包在 `({...})` 里，于是 `__always_inline` 调用点（典型是
        // `cpu_feature_enabled(...)`）就藏在里面。以前这一支落进下面的 `other => other`，
        // 里面没有任何调用点进过内联决策 —— 残留调用点逼编译器另发一份 callee 副本，
        // 而带 `"i"(形参)` 的副本编不出来（实测 `arch/x86/kernel/fpu/xstate.o` 报 E0311）。
        // 块体走与函数体**同一套** items 分派，不开第二份规则；预算按子块自己的体量算
        // （与 `BlockItem::Decl(Decl::Func)` 那条嵌函数同口径）。
        Expr::StmtExpr { block, span, ty } => {
            let mut block = block;
            let walk_sub = inline_walk_enabled() && arms_walkable(&block);
            inline_calls_in_items(
                &mut block.items,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth + 1,
                walk_sub,
            );
            Expr::StmtExpr { block, span, ty }
        },
        // 初始化列表/复合字面量里的调用点。内核大量写法是「结构体初始化器里就调用」：
        // `struct desc_ptr d = { .address = (unsigned long)rip_rel_ptr(table) }`
        // （`arch/x86/boot/startup/gdt_idt.c`）。以前这两支落在 `other => other`，
        // 于是 `__always_inline` 调用点留在初始化器里 —— 调用点没展开，编译器就得为
        // 它另发一份副本，而带 `"i"(形参)` 的副本编不出来。
        Expr::InitList { inits, span, ty } => Expr::InitList {
            inits: inits
                .into_iter()
                .map(|it| inline_expr_in_init_item(it, funcs, level, caller_flatten, caller_is_hot, fenv_access, depth))
                .collect(),
            span,
            ty,
        },
        Expr::CompoundLit { ty, inits, span } => Expr::CompoundLit {
            ty,
            inits: inits
                .into_iter()
                .map(|it| inline_expr_in_init_item(it, funcs, level, caller_flatten, caller_is_hot, fenv_access, depth))
                .collect(),
            span,
        },
        other => other,
    };
    fold_expr(e, fenv_access)
}

/// 初始化项里的表达式走同一条 [`inline_expr`]；指定初始化器的下标不展开
/// （`[f()] = x` 不是合法 C：C11 6.7.9 要求下标是常量表达式）。
fn inline_expr_in_init_item(
    it: InitItem,
    funcs: &InlineTable,
    level: u8,
    caller_flatten: bool,
    caller_is_hot: bool,
    fenv_access: bool,
    depth: u32,
) -> InitItem {
    match it {
        InitItem::Expr(e) => InitItem::Expr(inline_expr(
            e,
            funcs,
            level,
            caller_flatten,
            caller_is_hot,
            fenv_access,
            depth,
        )),
        InitItem::Designated { designator, value } => InitItem::Designated {
            designator,
            value: inline_expr(
                value,
                funcs,
                level,
                caller_flatten,
                caller_is_hot,
                fenv_access,
                depth,
            ),
        },
    }
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
        // G2 的原始诉求仍然成立：`out:`/`late:` **之后**各自的 `return s;` 必须还在
        //（落到函数尾 fall-off 会让 `%eax` 取到上一个标签里的残值）。
        //
        // 而 `goto out;` 与 `out:` 之间那条 `return 999;` 现在是**真不可达**、会被删掉：
        // 本轮给 `stopped` 补了 `goto`/`break`/`continue` 三条终止判据（内联会把被调方的
        // `return;` 改写成 `goto __inl<N>__end;`，只认 `Return` 时死尾段根本删不掉 ——
        // `arch/x86/mm/ioremap.o` 那 7 行 `efi_mem_type` 就是这个机制）。旧断言
        // 「≥ 3 个 Return」是那个实现缺口的产物，gcc 同样不发这条 `return 999`。
        assert_eq!(
            post_returns, 2,
            "G2: Label 之后各自的顶层 Return 必须保留（out后/late后各一条），而 goto 与首个 Label 之间的 `return 999` 属于不可达，实得 {post_returns} 个 Return"
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
                home: None,
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
        // 内联后 `sq`（static && always_inline）已无引用，DCE 会把它整个删掉，
        // 因此 `main` 不再固定在下标 1。按名字查找，不假设声明顺序。
        let main_body = module
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == "main" => f.body.as_ref(),
                _ => None,
            })
            .expect("main not found after inlining");
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

    /// 只给下面三条用例用的计数器：递归数一个函数体里 `Stmt::Label` / `Stmt::Goto` 各几条。
    /// 不在生产代码里复用，也不拿它当任何优化判据（生产代码的同一事实由
    /// [`rewrite_returns_to_dest`] 自己报）。
    fn count_labels_gotos(prog: &crate::ast::Program, fname: &str) -> (usize, usize) {
        fn count_block(b: &Block) -> (usize, usize) {
            let (mut l, mut g) = (0usize, 0usize);
            for it in &b.items {
                let (dl, dg) = match it {
                    BlockItem::Stmt(s) => count_stmt(s),
                    BlockItem::Decl(Decl::Func(f)) => {
                        f.body.as_ref().map(|bb| count_block(bb)).unwrap_or((0, 0))
                    },
                    _ => (0, 0),
                };
                l += dl;
                g += dg;
            }
            (l, g)
        }
        fn count_stmt(s: &Stmt) -> (usize, usize) {
            let mut sres = match s {
                Stmt::Label { stmt, .. } => {
                    let (l, g) = count_stmt(stmt);
                    return (l + 1, g);
                },
                Stmt::Goto { .. } => (0usize, 1usize),
                _ => (0usize, 0usize),
            };
            match s {
                Stmt::If { then_, else_, .. } => {
                    sres.0 += count_stmt(then_).0;
                    sres.1 += count_stmt(then_).1;
                    if let Some(e) = else_ {
                        let (l, g) = count_stmt(e);
                        sres.0 += l;
                        sres.1 += g;
                    }
                },
                Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                    let (l, g) = count_stmt(body);
                    sres.0 += l;
                    sres.1 += g;
                },
                Stmt::Block { b, .. } => {
                    let (l, g) = count_block(b);
                    sres.0 += l;
                    sres.1 += g;
                },
                Stmt::Switch { cases, prelude, .. } => {
                    for it in prelude {
                        if let BlockItem::Stmt(st) = it {
                            let (l, g) = count_stmt(st);
                            sres.0 += l;
                            sres.1 += g;
                        }
                    }
                    for c in cases {
                        let (l, g) = count_block(&c.body);
                        sres.0 += l;
                        sres.1 += g;
                    }
                },
                _ => {},
            }
            sres
        }
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.name.name == fname {
                    return f.body.as_ref().map(|b| count_block(b)).unwrap_or((0, 0));
                }
            }
        }
        (0, 0)
    }

    /// 拼接一个体内**没有 `return`** 的 `__always_inline` 助手时，不得留下
    /// 「没人引用的出口标签」。多发一条死标签会让整个 caller 函数被
    /// `ssa/structure.rs::stmt_has_unstructured_flow` 归为非结构化，而 `run_ssa_passes` 对
    /// 非结构化函数只走 analysis-only 分支（不回写 AST）⇒ 它的常量 `switch` 进不了
    /// SCCP，内核那族尺寸检查宏的 `default:` 陷阱调用就这么活到了链接期。
    #[test]
    fn general_inline_of_void_body_without_return_emits_no_exit_label() {
        let prog = optimize_src(
            b"extern void trap_v(void);\n\
              static inline __attribute__((always_inline)) void sw(int size) {\n\
              \tswitch (size) { case 4: break; case 8: break; default: trap_v(); break; }\n\
              }\n\
              void r(const volatile long *p) { sw((int)sizeof(*p)); }\n",
            2,
        );
        let (labels, gotos) = count_labels_gotos(&prog, "r");
        assert_eq!(
            (labels, gotos),
            (0, 0),
            "无 return 的 callee 拼接完不该有出口标签/跳转（实测旧行为是 1/0：\
             那条死标签把 r 踢出 SCCP 通道，trap_v 因此活到产物里）"
        );
    }

    /// 反向护栏（防「矫枉过正」）：callee 体内**有** `return` 时，出口标签与跳转必须还在 ——
    /// 否则目的地语句会被 callee 的提前跳出绕过。与 `g2_dce_block_retains_items_after_label`
    /// 的正反成对写法同纪律。调用点用 `t = half(t);`（`x = f(...)` 是已被接的形状），
    /// 不能写成 `return half(y) + 1;` —— 嵌在更大表达式里的调用根本不展开。
    #[test]
    fn general_inline_of_body_with_return_keeps_exit_label() {
        let prog = optimize_src(
            b"static inline __attribute__((always_inline)) int half(int x) {\n\
              \tif (x < 0) { return -1; }\n\treturn x / 2;\n}\n\
              int h(int y) { int t = y; t = half(t); return t + 1; }\n",
            2,
        );
        let (labels, gotos) = count_labels_gotos(&prog, "h");
        assert!(
            labels >= 1 && gotos >= 1,
            "有 return 的被拼接体必须保留出口标签与指向它的 goto（得 labels={labels} gotos={gotos}）"
        );
    }

    /// -O0/-O1 契约：这两个档位本就不走 `try_static_inline_general`（门在 `level < 2` 直接
    /// return false），所以出口标签/跳转都该是 0，caller 仍是一次真调用。
    #[test]
    fn general_inline_exit_label_gates_are_absent_below_o2() {
        let src = b"extern void trap_v(void);\n\
                    static inline __attribute__((always_inline)) void sw(int size) {\n\
                    \tswitch (size) { case 4: break; default: trap_v(); break; }\n\
                    }\n\
                    void r(const volatile long *p) { sw((int)sizeof(*p)); }\n";
        for level in [0u8, 1u8] {
            let prog = optimize_src(src, level);
            let (labels, gotos) = count_labels_gotos(&prog, "r");
            assert_eq!(
                (labels, gotos),
                (0, 0),
                "-O{level} 不应发生函数体级拼接（出口标签/跳转必为 0）"
            );
        }
    }

    /// `return f(...);` 且 callee 返回 **void**：callee 的体里没有 `return`（走完体就是
    /// 返回），展开块末尾必须补回 `return;`。丢掉的后果不是少一条优化 —— kbuild22 的
    /// `--boot` 就是死在 `lib/maple_tree.c::mas_topiary_replace` 的
    /// `if (mte_is_leaf(old_enode)) return mas_free(mas, old_enode);`：释放旧叶之后落入
    /// 第二循环，用死叶做 `mas_find_child`，读到叶的 `slots[15]`（NULL）后解引用地址 0。
    #[test]
    fn general_inline_void_callee_at_return_site_keeps_return() {
        let prog = optimize_src(
            b"extern int g;\n\
              static void f(int x) { g += x; }\n\
              void t(int leaf) { if (leaf) return f(1); g += 100; }\n",
            2,
        );
        let body = func_body(&prog, "t");
        assert!(
            block_has_valueless_return(body),
            "return f(); 展开后必须补回 return（否则会落进后面的 `g += 100;`）"
        );
    }

    /// 反向护栏：callee 返回**非 void** 时，展开块末尾不得画蛇添足地补 `return;` ——
    /// 那会把「callee 没走到 return 的路径」（UB 但不该改变其它路径的产物）变成
    /// 「提前归还一个未定义值」。
    #[test]
    fn general_inline_nonvoid_callee_at_return_site_keeps_value_return() {
        let prog = optimize_src(
            b"extern int g;\n\
              static int f(int x) { if (x) return 1; g += x; return 2; }\n\
              int t(int leaf) { if (leaf) return f(1); return 0; }\n",
            2,
        );
        let body = func_body(&prog, "t");
        assert!(
            !block_has_valueless_return(body),
            "非 void callee 的展开块末尾不该出现无值 return（callee 自己的 `return e;` 才是本层的返回）"
        );
    }

    /// 上面两条护栏取函数体用。
    fn func_body<'a>(prog: &'a crate::ast::Program, name: &str) -> &'a Block {
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.name.name == name {
                    return f.body.as_ref().expect("函数体");
                }
            }
        }
        panic!("找不到函数 {name}");
    }

    /// 函数体（含嵌套块/分支/循环/switch）里是否有无值 `return;`。写成递归而不是钉
    /// 某一层形状：`run_ssa_passes` 在 -O2 会重建 caller，块嵌套层数不是内联器的契约。
    fn block_has_valueless_return(b: &Block) -> bool {
        fn in_stmt(s: &Stmt) -> bool {
            match s {
                Stmt::Return { val: None, .. } => true,
                Stmt::Return { .. } | Stmt::Expr { .. } | Stmt::Goto { .. } => false,
                Stmt::GotoIndirect { .. }
                | Stmt::Break { .. }
                | Stmt::Continue { .. }
                | Stmt::Fallthrough { .. }
                | Stmt::EhLandingPad { .. }
                | Stmt::AsmGoto { .. } => false,
                Stmt::If { then_, else_, .. } => {
                    in_stmt(then_) || else_.as_ref().is_some_and(|e| in_stmt(e))
                },
                Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                    in_stmt(body)
                },
                Stmt::Label { stmt, .. } => in_stmt(stmt),
                Stmt::Invoke { call, .. } => in_stmt(call),
                Stmt::Block { b, .. } => block_has_valueless_return(b),
                Stmt::Switch { cases, prelude, .. } => {
                    prelude.iter().any(|it| match it {
                        BlockItem::Stmt(s) => in_stmt(s),
                        _ => false,
                    }) || cases.iter().any(|c| block_has_valueless_return(&c.body))
                },
            }
        }
        b.items.iter().any(|it| match it {
            BlockItem::Stmt(s) => in_stmt(s),
            _ => false,
        })
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

    /// 臂体展开预算：小函数放行、巨大函数挡住。
    ///
    /// 这一条盯的是本轮实测到的那件事：WALK 全开让 `sqlite3.c` 的峰值 RSS 推到 6.77GB
    /// 并被 OOM 杀死。预算不看展开次数（那要隐式全局状态，并行分块下不可复现），只看
    /// caller 体量 —— 一个纯函数，所以可单测、也不依赖 `OnceLock` 里那个上限的具体值。
    #[test]
    fn arm_budget_allows_small_callers_and_blocks_giant_ones() {
        // 取第一个 FuncDecl（文件作变量不在 `decls` 里，按名字找比按下标稳）。
        fn first_func(prog: &crate::ast::Program) -> &crate::ast::FuncDecl {
            prog.decls
                .iter()
                .find_map(|d| match d {
                    Decl::Func(f) if f.body.is_some() => Some(f),
                    _ => None,
                })
                .expect("夹具里应有一个带体的函数")
        }
        let (prog, _) = crate::parser::parse(
            b"int g; int f(int x){ if (x) g = 1; else g = 2; return g; }\n",
            "budget-small.c",
        );
        assert!(
            arm_budget_ok_for(first_func(&prog).body.as_ref().unwrap()),
            "几行的小函数必须能走臂体展开"
        );
        // 造一个巨大的 caller：6000 条独立赋值，节点数远超默认上限
        let mut src = String::from("int big(int x){ int a = 0;");
        for i in 0..6000 {
            src.push_str(&format!("a = a + {};", i % 7 + 1));
        }
        src.push_str("return a;}");
        let (prog, _) = crate::parser::parse(src.as_bytes(), "budget-big.c");
        let body = first_func(&prog).body.as_ref().unwrap();
        let nodes: usize = body.items.iter().map(item_node_count).sum();
        assert!(
            nodes > INLINE_ARM_BODY_NODES_DEFAULT,
            "夹具得真的超过上限才能测出拒绝（实得 {nodes}）"
        );
        assert!(!arm_budget_ok_for(body), "巨大 caller 必须被预算挡住（那是 OOM 的来源）");
    }

    /// 预算的量尺必须递归进**标签包住的体**。
    ///
    /// 这是评审直测出来的那个 fail-open：`stmt_node_count` 以前带 `_ => 1`，于是一个
    /// 把整段体括在 `L:{...}` 里的 caller 只被计成几个节点。Lua 的 `vmcase(l)` 展开就是
    /// `L_OP_MOVE: {…}`，所以「按 caller 体量限住才敢默认开」这句话在标签形语料上
    /// 完全不成立。本例就是钉这一条的回归判据。
    #[test]
    fn arm_budget_counts_statements_wrapped_in_a_label() {
        fn first_func(prog: &crate::ast::Program) -> &crate::ast::FuncDecl {
            prog.decls
                .iter()
                .find_map(|d| match d {
                    Decl::Func(f) if f.body.is_some() => Some(f),
                    _ => None,
                })
                .expect("夹具里应有一个带体的函数")
        }
        // 同样的体，一种直接平铺、一种括在标签下：两者节点数必须同一量级。
        let mut flat = String::from("int f(int x){ int a = 0;");
        let mut labeled = String::from("int f(int x){ int a = 0; L0: {");
        for i in 0..1200 {
            let s = format!("a = a + {};", i % 7 + 1);
            flat.push_str(&s);
            labeled.push_str(&s);
        }
        flat.push_str("return a;}");
        labeled.push_str("} return a;}");
        let (pf, _) = crate::parser::parse(flat.as_bytes(), "budget-flat.c");
        let (pl, _) = crate::parser::parse(labeled.as_bytes(), "budget-label.c");
        let n_flat = arm_body_nodes(
            pf.decls
                .iter()
                .find_map(|d| match d {
                    Decl::Func(f) => f.body.as_ref(),
                    _ => None,
                })
                .unwrap(),
        );
        let body_l = first_func(&pl).body.as_ref().unwrap();
        let n_label = arm_body_nodes(body_l);
        assert!(
            n_label.saturating_mul(2) >= n_flat,
            "标签形不得只量出平铺形的一半以下（实得 标签={n_label} 平铺={n_flat}）"
        );
        assert!(n_label > INLINE_ARM_BODY_NODES_DEFAULT, "夹具得真的超上限（实得 {n_label}）");
        assert!(!arm_budget_ok_for(body_l), "括在标签里的巨大体同样必须被预算挡住");
    }

    /// `switch` 的 `prelude` 也是 caller 体的一部分，不得从预算量尺里漏掉。
    #[test]
    fn arm_budget_counts_switch_prelude() {
        fn body_of(prog: &crate::ast::Program) -> &Block {
            prog.decls
                .iter()
                .find_map(|d| match d {
                    Decl::Func(f) => f.body.as_ref(),
                    _ => None,
                })
                .expect("夹具里应有一个带体的函数")
        }
        let pad: String = (0..1200)
            .map(|i| format!("a = a + {};\n", i % 7 + 1))
            .collect();
        // 两份同体积的体：一份平铺在 switch 之前，一份真的放进 `prelude`。漏计 `prelude`
        // 时两者的节点数会差出一整个 pad（而不是像旧版本那样只测平铺那一份 —— 它对
        // 「prelude 进不进账」零约束）。
        let flat = format!(
            "int f(int x){{ int a = 0; {pad} switch (a) {{ case 1: return 2; }} return a;}}"
        );
        let pre = format!(
            "int f(int x){{ int a = 0; switch (a) {{ {pad} case 1: return 2; }} return a;}}"
        );
        let (pf, _) = crate::parser::parse(flat.as_bytes(), "budget-flat2.c");
        let (pp, _) = crate::parser::parse(pre.as_bytes(), "budget-prelude.c");
        let n_flat = arm_body_nodes(body_of(&pf));
        let n_pre = arm_body_nodes(body_of(&pp));
        assert!(
            n_pre.saturating_mul(2) >= n_flat,
            "prelude 里那 1200 条必须与平铺同量级（实得 prelude={n_pre} 平铺={n_flat} ⇒ prelude 被漏计）"
        );
        assert!(n_pre > INLINE_ARM_BODY_NODES_DEFAULT, "夹具得真的超上限（实得 {n_pre}）");
        assert!(!arm_budget_ok_for(body_of(&pp)), "prelude 巨大的 caller 必须被预算挡住");
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
                    home: None,
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
                    home: None,
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

    /// `push_asm_idents` 只收「符号位」上的名字：助记符（含 `rep`/`ds` 这类指令前缀）、
    /// 标号定义、`#` 注释内容都不算。
    ///
    /// 这条判据决定跨函数 DCE 的根集：收多了会把整个 TU 的 `static inline` 钉活。
    /// 实测 vDSO 因此死在 `undefined reference to do_trace_write_msr` —— `wrmsr` 指令
    /// 与 `special_insns.h` 里的 `static inline void wrmsr()` 同名。
    #[test]
    fn asm_ident_scan_skips_mnemonics_prefixes_and_comments() {
        let mut out = std::collections::HashSet::new();
        push_asm_idents(
            "1: wrmsr\n2:\n .pushsection __ex_table, \"a\"\n .long (1b) - .\n .popsection\n",
            &mut out,
        );
        push_asm_idents("rep insb", &mut out);
        push_asm_idents("771:\n\tds clflush %0\n", &mut out);
        push_asm_idents("1: # ALT: oldinstr\n772:\n", &mut out);
        push_asm_idents("jmp l_yes", &mut out);
        push_asm_idents(".quad my_target + 8", &mut out);
        push_asm_idents("call *%rax", &mut out);
        for gone in [
            "wrmsr", "insb", "clflush", "oldinstr", "ALT", "rep", "ds", "rax", "b",
        ] {
            assert!(!out.contains(gone), "{gone} 不该被当成符号引用：{out:?}");
        }
        for kept in ["__ex_table", "l_yes", "my_target"] {
            assert!(out.contains(kept), "{kept} 是符号引用，不能被跳掉：{out:?}");
        }
    }
}
