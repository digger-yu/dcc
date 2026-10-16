// src/ssa/mod.rs - 轻量 SSA IR 子系统 (M16 优化 pass 基础设施) 的入口
//
// 设计原则 (suckless):
//   - 只覆盖结构化控制流 (if/while/for/do-while/return)。含 goto/label/asm-goto
//     /switch(Switch 多 case 简化为 if-chains)/longjmp 的函数降级回 AST pass。
//   - 单一真源: 同一条判据只在一个文件里定义一次, 别处引用, 不复制。
//   - 零外部依赖, 仅 std。
//   - IR 形态: 每个变量按 SSA 版本号命名 (base.N); phi 节点在 merge BB 顶端。
//
// 地图 —— 每个子文件放什么, 以及**不放什么** (改代码前先在这里定位):
//   ir.rs            IR 数据形态 (SsaVar/Operand/InstKind/Terminator/BasicBlock/Phi/
//                    OrderedNameSet/Function) 与其属性判据 (kind_operands/inst_reads_memory/
//                    inst_unremovable/inst_clobbers_memory/inst_written_bases/collect_uses_*/
//                    rename_* 的 SSA 引用改写基础设施)。不放遍历、不放降级、不放重建。
//   cfg.rs           CFG 与支配: term_succs / collect_all_preds / compute_dominators /
//                    compute_ipdoms / is_back_edge / reverse_postorder / refresh_cfg /
//                    find_loop_body{,_multi} / is_cycle_entry / predecessor_counts /
//                    ordered_block_names (确定性遍历序) / dangling_refs / duplicate_ssa_defs。
//                    不放合法性判据以外的任何 IR 改写。
//   structure.rs     进 SSA **之前**的 AST 侧政策: is_structured* 准入判据 +
//                    lower_switches* (switch → if-chain)。降级政策只在这里, 不要在
//                    rebuild.rs 复制判据。不放 SSA → AST 的重建。
//   build.rs         AST → SSA 的唯一入口: `Builder` 与其降级政策本体 +
//                    build_from_func / build_ssa / expr_shape。不放左值降级细节与 phi 插入。
//   lower.rs         被 Builder 调用的降级辅助: lower_lvalue / collect_local_decls{,_stmt} /
//                    insert_loop_phis / bump_version / insert_label_phis。政策本体在 build.rs。
//   loop_exit.rs     多出口循环的出口归属 —— 判据与发射器的同一真源。has_multi_exit_cycle /
//                    loop_exit_plan / loop_exit_owner / loop_shape 与 rebuild.rs 的各出口边
//                    发射点必须调同一个 loop_exit_plan, 改一边必须同时看另一边 (§7.28/§7.32)。
//                    不放出口边的实际发射。
//   inline_temps.rs  合成临时值的内联窗口: plan_inlined_temps / inline_window_safe /
//                    is_synthetic_temp_base / find_use_site。不放出口归属与表达式翻译。
//   rebuild.rs       SSA → AST 的**控制流**重建: rebuild_block / RebuildCtx / rebuild_bb /
//                    rebuild_cycle / walk_to_join / arm_join / branch_join /
//                    emit_rebuild_inst。
//                    单条指令 → 表达式的翻译不在这里。
//   expr_rebuild.rs  SSA → AST 的**表达式**重建与类型推导: inst_to_expr{,_resolved} /
//                    operand_to_expr / infer_binop_type / store_pointee_ty / addr_of_ty /
//                    unary_result_ty / int_const_ty。类型推导判据只在这里, 不要在 rebuild.rs
//                    再写一份。
//   dump.rs          dump_function —— `DCC_SSA_DUMP` 的读数通道; 诊断文案只在这里。
//   location.rs      SSA 值的**家** (`Location`/`LocMap`/`RegBank`) —— P12 字面形态的载体。
//                    只定义数据形态与显式改写入口 (fork/rename/retain)；**不放**任何
//                    分配策略（活性、干涉、溢出决策全在 regalloc.rs），也不放后端编号到
//                    物理寄存器的解释（那是 abi/regbank.rs 的事）。
//   liveness.rs      逐点活性与**干涉表**（Phase 5.1 的数据地基）。只回答「这两个值会
//                    同时活着吗」，不放「该给哪个号」的决策（那在 regalloc.rs）。
//   tests.rs / unary_rebuild_ty_tests.rs
//                    跨切用例 (构造 → 跑 pass → 重建检查产物形态) 集中放这里, 不为了
//                    「文件对齐」把一条回归场景切成两半 —— 那会同时毁掉它的可读性与判据价值。
pub mod build;
pub mod cfg;
pub mod dump;
pub mod expr_rebuild;
pub mod inline_temps;
pub mod ir;
pub mod liveness;
pub mod location;
pub mod loop_exit;
pub mod lower;
// passes.rs        M16 优化 pass 集合 (P1..Pn: 跨函数 DCE / 强度削减 / IVOP / GVN / 调度…),
//                  本目录之外的消费者只走 run_with_opts / cross_function_dce_with_roots。
pub mod passes;
// pipelining.rs    P16 软件流水: 自然循环依赖图 + 模调度 + prologue/kernel/epilogue 三段计划
//                  (只分析与出计划, 不碰 IR)。
pub mod pipelining;
pub mod rebuild;
// regalloc.rs      P12 SSA 层寄存器分配: 活性只在 IR 上算得准, 结果落进重建后的 AST 绑定表。
pub mod regalloc;
pub mod structure;

#[cfg(test)]
mod tests;
/// 一元指令重建必须把结果类型定成指令 `dest` 的类型。
///
/// 旧实现只认向量类型，其余一律写 `Type::Int`，于是 `double n = -b;` 重建后成了
/// `Unary{ty: Int}`，codegen 靠 `e.ty()` 判走不走 xorpd 的分支落回整数 `neg r64`
/// ⇒ 浮点取负直接丢失（实测 -O2 打印 `0.000000`，真值 `-2.300000`）。
#[cfg(test)]
mod unary_rebuild_ty_tests;

// ============================================================================
// 对外接口 (外部调用方一律走 `crate::ssa::*`, 不必知道内容在哪个子文件里)
//
// 下面的 `pub use` 只列**本目录之外真有消费者**的名字。其余项用普通
// `use` 在 `ssa` 内部再导出: 兄弟模块仍按 `crate::ssa::X` 取用 (子模块看得见祖先
// 模块的私有导入), 但它们不再是 lib crate 的公开面。
// 例外: `OrderedNameSet` 必须保持 `pub` —— 它是 `pub struct Function` 的 `pub`
// 字段 (`original_locals`) 的类型, 收窄会构成「私有类型出现在公开接口」。
// ============================================================================

pub use ir::{
    BasicBlock, Function, Inst, InstKind, Operand, OrderedNameSet, Phi, SsaVar, Terminator,
};
// `LocMap` 是 `pub struct Function` 的 `pub` 字段类型，与 `OrderedNameSet` 同理必须保 `pub`；
// 其余三个是 `Location` 的组成件，后端消费时要按名字读，所以也对外。
pub use build::build_from_func;
pub use dump::dump_function;
pub use location::{LocMap, Location, RegBank, RegId, Splits};
pub use loop_exit::has_multi_exit_cycle;
pub use rebuild::{
    rebuild_block, rebuild_broke_lazy_guard, rebuild_expansion_peak, rebuild_over_budget,
    rebuild_steps,
};
pub use structure::lower_switches_in_func;

// 仅在 `ssa` 子系统内部共享的判据与工具 (收编 passes/pipelining/regalloc 后，
// 它们的消费者已都是兄弟模块)。下面只保留「兄弟模块确实按 `crate::ssa::X`
// 取用」的那几条再导出；其余内部项走 `crate::ssa::<子模块>::X` 直接路径，
// 不需要在这里占位（多占一条会撞 unused_imports）。
use cfg::{
    collect_all_preds, compute_dominators, dangling_refs, duplicate_ssa_defs, find_loop_body,
    ordered_block_names, refresh_cfg, term_succs_pub,
};

// IR 级基础设施: `InstKind` 操作数位置的**穷举**枚举, 各 pass 不得自己另写一份
// (漏一个位置就留下指向已删定义的悬空引用)。同样只在 `ssa` 内消费。
use ir::kind_operand_ssa_vars;
