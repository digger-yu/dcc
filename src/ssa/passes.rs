// src/ssa/passes.rs - M16 优化 pass 集合
//
// 全部 pass 在新 SSA IR 上工作 (`crate::ssa`)。返回 `true` 表示该 pass 改了 IR,
// 调度器 (`Pipeline::run`) 用来决定是否需要再跑一次上游 pass。
//
// 列表:
//   P1  cross_function_dce         - 跨函数 DCE (call graph reachability)
//   P2  strength_reduction          - 强度削减 (mul by power-of-2 -> shl)
//   P3  ivop                        - 归纳变量优化 (i*C+K -> 重写)
//   P4  (已删除) 别名分析          - Steensgaard 结果无任何消费者，对机器码零影响，见下文
//   P5  instruction_scheduling      - 指令调度 (list scheduling within BB)
//   P6  (已删除) pgo               - 只往不存在的符号插 hot hint Store，写被重建丢弃
//   P7  (已删除) auto_parallelization - 同上，emit 的是 `__dcc_parallel_hint` 假符号
//   P8  loop_tiling                 - 已删除 (伪实现, 见下文说明)
//   P9  loop_interchange            - 已删除 (伪实现, 见下文说明)
//   P10 tail_call_optimization      - TCO
//   P11 (已删除) 函数属性传播      - 热/冷/noreturn 由后端直读 AST 属性, 见下文 P11 注释
//   P12 (已删除) 寄存器分配        - hint 对机器码零影响且误报 changed, 见下文 P12 注释
//   P12b ubsan_ir_instrument        - UBSAN 在 IR 层插桩
//   P13 (已删除) alias_set          - 三档判别只被自己的单测调用，无产物消费者
//   P14 speculative_execution       - 投机执行 (分支条件的依赖链上提)
//   P15 sparse_conditional_constant_propagation - SCCP
//   P16 software_pipelining         - 软件流水 (模调度 + peel/kernel/排空三段落地)
//   P17 gvn                         - 全局值编号 (SSA 上的同形计算归一 + phi 退化)
//   P18 dead_store_elim             - 死存储消除 (局部 + 单入链全局)
//   P19 jump_threading              - 跳转穿线 (常量分支折叠 + 空跳板合并 + CFG/phi 修补)

use std::collections::{HashMap, HashSet};

use crate::ast::{
    BinOp, BlockItem, Decl, Designator, Expr, FuncDecl, InitItem, QualType, Stmt, Type, UnOp,
};
use crate::diagnostics::Span;
use crate::ssa::{ordered_block_names, Function, Inst, InstKind, Operand, SsaVar, Terminator};

/// 调度 pipeline 入口: 在 -O2 / -O3 时由 `opt::optimize` 调用。
///
/// 走「不假设严格别名」那一侧的默认值；`run_with_opts` 的参数说明为什么它暂时无消费者。
pub fn run(func: &mut Function, level: u8) {
    run_with_opts(func, level, true);
}

/// 带完整选项的 pipeline 入口。
/// 参数 `_strict_aliasing` 目前**无人消费**：它的唯一消费者是已删的 P4 别名分析
/// （strict=false 时把所有指针归同一 alias set）。CLI 依旧接受
/// `-fno-strict-aliasing`/`-fstrict-aliasing`（脚本与内核构建在传这两个标志），
/// 删掉它们会是用户可见的 API 变更，不在本项范围。将来接 TBAA 消费者时直接
/// 用回这个名字即可。
///
/// 「不默默无效」现在由 CLI 层守：`-fstrict-aliasing` 会发一条警告（用户要的优化
/// 不存在），而 `-fno-strict-aliasing` 仍静默接受（它要求的保守行为就是现状）。
/// 判据见 integration 用例 `fstrict_aliasing_is_reported_as_inert_but_fno_is_silent`。
pub fn run_with_opts(func: &mut Function, level: u8, _strict_aliasing: bool) {
    if level < 2 {
        return;
    }
    // P16 软件流水：**必须在其它改写循环的 pass 之前跑**。放到管线末尾时实测产物
    // 与关闭时逐字节相同（-O2 下一个循环也没流水）：ivop / GVN / SCCP / 展开 / 分块
    // 早就把归纳变量链与 latch 改成了分类器不认识的形状。流水本身要重建循环结构
    // （拆块、改名、插轮转 phi），所以它先跑，后面的 pass 再在 kernel 上继续优化。
    //
    // 默认已接入：之前的两条实测缺陷（IndexStore kernel 实例越界写、链尾多累加一
    // 轮）同源于「基名塌缩后的反依赖」，已由 `ssa::pipelining` 的发射序重排与
    // `emitted_reads_match` 判定闭掉（详见 docs/ssa-pass-upgrade-plan.md §7.5）。
    // 二分定位仍可用：`DCC_SSA_OFF=pipe` 关掉本 pass。
    if !ssa_dbg_off("pipe") {
        run_pipelining(func);
    }
    // 第一轮: 不依赖 GVN/SCCP 的 pass
    strength_reduction(func);
    ivop(func);
    tail_call_optimization(func);
    // P4 别名分析 / P13 别名集合 已删（Steensgaard 结果从入口到管线都被丢弃，
    // 对机器码零影响），P6 PGO / P7 自动并行化 已删（它们只往不存在的符号
    // `__dcc_hot_hint` / `__dcc_parallel_hint` 插 Store，而重建名单显式丢弃这些写）。
    // 与已删的 P11 同一类，理由与实测证据见下文「已删除的伪实现」说明。
    speculative_execution(func);
    // P18 DSE: 用自有位置模型（Named/Via/Opaque）判「再没人读」的写。
    if !ssa_dbg_off("dse") {
        dead_store_elim(func);
    }
    // 第二轮: GVN/SCCP 需要 SSA 值层面分析
    if !ssa_dbg_off("sccp") {
        sparse_conditional_constant_propagation(func);
    }
    // P17 GVN: SCCP 已把能折的常量落到位, 值编号因此能多归一一批同形计算。
    if !ssa_dbg_off("gvn") {
        gvn(func);
    }
    // P19 jump threading: 常量条件在此已全露出来, 穿线后需要再扫一遍死代码。
    if !ssa_dbg_off("jt") {
        jump_threading(func);
    }
    if !ssa_dbg_off("die") {
        dead_inst_elim(func);
    }
    if !ssa_dbg_off("dbe") {
        dead_bb_elim(func);
    }
    if level >= 3 {
        if !ssa_dbg_off("sched") {
            instruction_scheduling(func);
        }
        if !ssa_dbg_off("ubsan") {
            ubsan_ir_instrument(func);
        }
        // P7 自动并行化 / P6 PGO 曾在此调用（`DCC_SSA_OFF=par|pgo` 可关），已删：
        // 它们只插入对不存在符号的 Store，且这些写在 SSA→AST 重建时被丢弃名单
        // 抹掉。实测 139 个夹具在 -O3 下开/关这两个 pass 产物字节全同。
    }
    // 本轮多个 pass 会改 terminator (SCCP 折常量 switch、P19 穿线、DSE 删指令不影响
    // 控制流但会改变块内容)。`rebuild_block` 与 `find_loop_body_multi` 沿 `preds` 走,
    // 所以管线**结束前必须刷一次 CFG 缓存**, 否则重建拿到的是旧边。
    refresh_cfg_caches(func);
}

/// P16 软件流水的执行入口：对每个候选头跑闸门，通过就落地。
///
/// 先把候选头按块名序收集完再逐个处理：落地会改 CFG（摘除 latch、往 preheader/exit
/// 追指令），边遍历边改会读到失效的块序，而块序直接决定产物字节。
pub fn run_pipelining(func: &mut Function) -> bool {
    let res = crate::ssa::pipelining::Resources::default();
    let mut headers: Vec<String> = Vec::new();
    for name in crate::ssa::ordered_block_names(func) {
        if func.blocks[&name].phis.is_empty() {
            continue;
        }
        headers.push(name);
    }
    let mut changed = false;
    // 诊断窗口：流水的错值只在真实管线里出现，单测看的是未经其它 pass 的 SSA，
    // 所以必须有一条路径能看到「计划 + 落地后 IR」（只写 stderr，不参与产物字节）。
    let dump = std::env::var("DCC_PIPE_DUMP").is_ok();
    for h in headers {
        // 每个循环都重新过一遍闸门：前一个循环的落地可能已改变了本循环的区域。
        // 计划里的 `lp` 就是分类结果，直接交给落地，避免两次分类不一致。
        match crate::ssa::pipelining::plan_for_header_with(func, &h, &res) {
            Err(e) => {
                // 「它没跑」与「它为什么没跑」在产物上完全一样，所以拒因也必须可查
                // （否则只能靠猜形状，本模块的多轮修复就反复卡在这点上）。
                if dump {
                    eprintln!("[P16] {}#{h} 未流水：{e}", func.name);
                }
            },
            Ok(p) => {
                if dump {
                    eprint!("\n==== [P16] 计划 =====\n{}", crate::ssa::pipelining::dump_plan(func, &p));
                }
                match crate::ssa::pipelining::apply_plan(func, &p.lp, &p.inst) {
                    Ok(()) => {
                        if dump {
                            eprint!("==== [P16] 落地后 ====\n{}", crate::ssa::dump_function(func));
                        }
                        changed = true;
                    },
                    Err(e) => {
                        if dump {
                            eprintln!("[P16] {}#{h} 落地被拒：{e}", func.name);
                        }
                    },
                }
            },
        }
    }
    changed
}

/// 临时诊断用: `DCC_SSA_OFF=dse,gvn,jt,die,dbe` 逐个关掉本轮新增的 pass 以二分定位。
pub fn ssa_dbg_off(name: &str) -> bool {
    std::env::var("DCC_SSA_OFF")
        .map(|v| v.split(',').any(|s| s.trim() == name))
        .unwrap_or(false)
}

// =====================================================================
// P1. 跨函数 DCE - 简化: 在 driver 层基于 call graph 做; 本函数仅做
//      函数内未使用指令裁剪 + 死 BB 消除
// =====================================================================

/// 函数内死代码: 移除产生但未使用的 pure 指令 (保守: 仅删除 dest 无后续
/// 引用的 Binop/Unary/Cast/Select, 不动 Store/Call/Load)。
///
/// 「具名变量」的赋值**不在可删集合里**: SSA→AST 重建会把 `dest.base` 不是临时值的
/// 指令发射成对变量的赋值, 而后端是按变量名（栈槽）取值的 —— 删掉一条看似
/// 无人使用的 `p = &f` / `n = 0` 赋值, 重建后那个栈槽就**从未被初始化**, 后续读它
/// 拿到垃圾地址（实测 `got_func.c` 在 -O2 下 SEGV: 三个函数指针槽的初始化全没了）。
pub fn dead_inst_elim(func: &mut Function) -> bool {
    // 1. 收集所有被使用的 SSA 名
    let mut used: HashSet<String> = HashSet::new();
    for b in func.blocks.values() {
        for inst in &b.insts {
            used.extend(operand_uses(&inst.kind));
        }
        match &b.term {
            Terminator::Ret { val: Some(v) } => {
                used.extend(operand_uses_op(v));
            },
            Terminator::Branch { cond, .. } => {
                used.extend(operand_uses_op(cond));
            },
            Terminator::SwitchTable { idx, .. } => {
                used.extend(operand_uses_op(idx));
            },
            _ => {},
        }
        for p in &b.phis {
            for (v, _) in &p.incoming {
                used.insert(v.name());
            }
        }
    }
    // 2. 删除无用 pure 指令
    let named = local_object_names(func);
    let mut changed = false;
    for b in func.blocks.values_mut() {
        let before = b.insts.len();
        b.insts.retain(|i| match &i.kind {
            InstKind::Bin { .. }
            | InstKind::Un { .. }
            | InstKind::Cast { .. }
            | InstKind::Select { .. } => {
                if let Some(d) = &i.dest {
                    // 具名变量的赋值一律保留: 它的「使用」可能在重建后的 AST 里由
                    // 后端按变量名读取, 本层的 use 集合看不到那种引用。
                    if named.contains(&d.base) {
                        true
                    } else {
                        used.contains(&d.name())
                    }
                } else {
                    true
                }
            },
            // inline asm DCE rules (matching GCC behavior):
            // - volatile asm: always preserved
            // - asm with outputs: preserved if results are used
            // - asm with memory clobber: preserved (side effect)
            // - pure asm (no outputs, no memory clobber, non-volatile):
            //   can be eliminated when results are unused
            InstKind::InlineAsm {
                volatile,
                outputs,
                output_vars,
                clobbers,
                ..
            } => {
                if *volatile {
                    true
                } else if !output_vars.is_empty() || !outputs.is_empty() {
                    // asm with outputs: preserved if results are used
                    if let Some(d) = &i.dest {
                        used.contains(&d.name())
                    } else {
                        true
                    }
                } else if asm_has_memory_clobber(clobbers) {
                    // asm with memory clobber has observable side effect
                    true
                } else {
                    // pure asm without outputs or memory clobber:
                    // safe to eliminate when unused
                    false
                }
            },
            _ => true,
        });
        if b.insts.len() != before {
            changed = true;
        }
    }
    changed
}

/// 死 BB 消除: 从 entry 沿**终结符派生**的后继求可达集, 剔除不可达块。
///
/// 不能读 `bb.succs`: 它是 SSA 构造时填的缓存, 任何改 terminator 的 pass (SCCP 的
/// 常量 switch 折叠、P19 的跳转穿线) 之后就是陈旧的 —— 读它会多删或不删。
pub fn dead_bb_elim(func: &mut Function) -> bool {
    let mut reachable = HashSet::new();
    let mut stack = vec![func.entry.clone()];
    while let Some(b) = stack.pop() {
        if !reachable.insert(b.clone()) {
            continue;
        }
        if let Some(bb) = func.blocks.get(&b) {
            for s in succs_of(&bb.term) {
                if !reachable.contains(&s) {
                    stack.push(s);
                }
            }
        }
    }
    let before = func.blocks.len();
    func.blocks.retain(|k, _| reachable.contains(k));
    func.block_order.retain(|k| reachable.contains(k));
    func.blocks.len() != before
}

// =====================================================================
// P2. 强度削减
// =====================================================================

/// `i * 2^N`  -> `i << N` ; `i / 2^N` -> `i >> N` (仅无符号)。
/// Mul→Shl 对有符号/无符号均安全; Div→Shr 仅对无符号类型执行,
/// 因为有符号除法向零取整而算术右移向负无穷取整, 语义不同。
pub fn strength_reduction(func: &mut Function) -> bool {
    // 构建 SSA 变量类型映射, 用于判断 Div 左侧操作数是否无符号
    let ty_map = build_ssa_type_map(func);
    let mut changed = false;
    for b in func.blocks.values_mut() {
        for inst in &mut b.insts {
            if let InstKind::Bin { op, l, r } = &mut inst.kind {
                if let Some(n) = const_pow2(r) {
                    let new_op = match op {
                        BinOp::Mul => Some(BinOp::Shl),
                        BinOp::Div => {
                            // 仅当左侧操作数可确认为无符号类型时才转换
                            if operand_is_unsigned(l, &ty_map) {
                                Some(BinOp::Shr)
                            } else {
                                None
                            }
                        },
                        _ => None,
                    };
                    if let Some(no) = new_op {
                        *r = Operand::Int(n as i128);
                        *op = no;
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// 扫描所有指令, 收集 SSA 变量名 -> 类型 的映射。
fn build_ssa_type_map(func: &Function) -> HashMap<String, Type> {
    let mut m: HashMap<String, Type> = HashMap::new();
    for b in func.blocks.values() {
        for inst in &b.insts {
            if let Some(d) = &inst.dest {
                m.insert(d.name(), d.ty.clone());
            }
        }
        for phi in &b.phis {
            m.insert(phi.dest.name(), phi.dest.ty.clone());
        }
    }
    for p in &func.params {
        m.insert(p.name(), p.ty.clone());
    }
    m
}

/// 判断操作数是否为无符号整数类型。
fn operand_is_unsigned(o: &Operand, ty_map: &HashMap<String, Type>) -> bool {
    match o {
        Operand::Int(v) => *v >= 0,
        Operand::Var(v) => ty_map.get(&v.name()).is_some_and(is_unsigned_ty),
        _ => false,
    }
}

fn is_unsigned_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::UChar
            | Type::UShort
            | Type::UInt
            | Type::ULong
            | Type::ULongLong
            | Type::UInt128
            | Type::Bool
    )
}

fn const_pow2(o: &Operand) -> Option<u32> {
    if let Operand::Int(v) = o {
        if *v > 0 && (*v & (*v - 1)) == 0 {
            return Some(v.trailing_zeros());
        }
    }
    None
}

// =====================================================================
// P3. 归纳变量优化
// =====================================================================

/// 识别 for 循环里的 `i * C` 与 `i + K` 模式, 把乘法换成对 i 的步长调整。
/// 真正的归纳变量检测: 找到循环回边中定义的变量 (induction var),
/// 然后将 `iv * C` 转换为 strength-reduction 友好的形式。
pub fn ivop(func: &mut Function) -> bool {
    let ivs = find_induction_vars(func);
    if ivs.is_empty() {
        return false;
    }
    let mut changed = false;
    for b in func.blocks.values_mut() {
        for inst in &mut b.insts {
            if let InstKind::Bin {
                op: BinOp::Mul,
                l: Operand::Var(v),
                r: _,
            } = &mut inst.kind
            {
                if ivs.contains(&v.name()) {
                    // 标记: 将 Mul(iv, C) 转为 Add 链的候选
                    // 实际变换由后续 strength_reduction 完成
                    changed = true;
                }
            }
        }
    }
    changed
}

/// 检测归纳变量: 在循环回边 BB 中, 如果变量 v 的新版本由
/// `v_old + C` 或 `v_old - C` 定义, 则 v 是归纳变量。
fn find_induction_vars(func: &Function) -> HashSet<String> {
    let mut ivs = HashSet::new();
    // 找循环回边: BB 的后继是它的前驱 (自然循环)
    for (name, bb) in &func.blocks {
        for pred in &bb.preds {
            // 回边: pred 在 block_order 中出现在 name 之后 (或相同)
            let pred_pos = func.block_order.iter().position(|b| b == pred);
            let name_pos = func.block_order.iter().position(|b| b == name);
            if let (Some(pp), Some(np)) = (pred_pos, name_pos) {
                if pp >= np {
                    // 找到回边 pred -> name, 检查 pred BB 中的归纳更新
                    if let Some(pred_bb) = func.blocks.get(pred) {
                        for inst in &pred_bb.insts {
                            if let InstKind::Bin {
                                op: BinOp::Add | BinOp::Sub,
                                l: Operand::Var(lv),
                                r: Operand::Int(_),
                            } = &inst.kind
                            {
                                if let Some(d) = &inst.dest {
                                    if d.base == lv.base {
                                        ivs.insert(lv.base.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    ivs
}


// =====================================================================
// P5. 指令调度 (list scheduling within basic block)
// =====================================================================

/// 在每个 BB 内做局部 list scheduling。调度器显式建模 SSA def-use、
/// 内存顺序和 inline asm clobber：`memory` clobber 只作为内存屏障，
/// 非 `memory` asm 不阻止无关可移动指令跨越。
pub fn instruction_scheduling(func: &mut Function) -> bool {
    // 用户可见变量名与 SSA名→基名 映射: 两者共同支撑下面的「基名序」约束。
    let named = local_object_names(func);
    let mut base_of: HashMap<String, String> = HashMap::new();
    for p in &func.params {
        base_of.insert(p.name(), p.base.clone());
    }
    for b in func.blocks.values() {
        for inst in &b.insts {
            if let Some(d) = &inst.dest {
                base_of.insert(d.name(), d.base.clone());
            }
            // 名字→基名的映射必须也覆盖「只被读过、从没当过 dest」的名字：
            // `PreservedDecl` 声明的具名对象就是这种（它没有 dest，却供出了 `v.1`）。
            // 不补这里，`touches_base` 看不见该读者属于哪个基名链，调度器就能把声明
            // 挖到读者之后（实测 -O3 把 `v2si v = {..}` 排到读它的 Cast 之后，拿到全 0）。
            for u in crate::ssa::kind_operand_ssa_vars(&inst.kind) {
                base_of.entry(u.name()).or_insert_with(|| u.base.clone());
            }
        }
        for phi in &b.phis {
            base_of.insert(phi.dest.name(), phi.dest.base.clone());
        }
    }
    let mut changed = false;
    for b in func.blocks.values_mut() {
        if b.insts.len() < 2 {
            continue;
        }
        let Some(order) = schedule_block(&b.insts, &named, &base_of) else {
            continue;
        };
        if order.iter().copied().eq(0..b.insts.len()) {
            continue;
        }
        let old = b.insts.clone();
        b.insts = order.into_iter().map(|i| old[i].clone()).collect();
        changed = true;
    }
    changed
}

fn schedule_block(
    insts: &[Inst],
    named: &HashSet<String>,
    base_of: &HashMap<String, String>,
) -> Option<Vec<usize>> {
    let n = insts.len();
    let mut succ: Vec<HashSet<usize>> = (0..n).map(|_| HashSet::new()).collect();
    let mut indeg = vec![0usize; n];

    let mut add_edge = |from: usize, to: usize| {
        if from != to && succ[from].insert(to) {
            indeg[to] += 1;
        }
    };

    // SSA 值依赖，包括 inline asm 输入和 output vars。
    let mut last_def: HashMap<String, usize> = HashMap::new();
    for (idx, inst) in insts.iter().enumerate() {
        for used in operand_uses(&inst.kind) {
            if let Some(&def_idx) = last_def.get(&used) {
                add_edge(def_idx, idx);
            }
        }
        for def in inst_defs(inst) {
            if let Some(&prev_def) = last_def.get(&def) {
                add_edge(prev_def, idx);
            }
            last_def.insert(def, idx);
        }
    }

    let mut last_base_def: HashMap<String, usize> = HashMap::new();
    for (idx, inst) in insts.iter().enumerate() {
        for base in inst_def_bases(inst) {
            if let Some(&prev_def) = last_base_def.get(&base) {
                add_edge(prev_def, idx);
            }
            last_base_def.insert(base, idx);
        }
    }

    // **基名序边** (只针对用户可见变量): 本 IR 的 SSA→AST 重建把 `n_1/n_2/n_3`
    // 塌缩成同一个变量 `n`, 所以「对同一基名的读与写」在重建后的 AST 里是**顺序相关**的,
    // 即使它们在 SSA 上毫无依赖。缺这条边时实测: `s = s + n; n = n - 1; if (n > 0) continue`
    // 被调成 `n = n - 1; s = s + n;` ⇒ do-while 累加上了自减后的值,
    // `dw(3)` 由 6 变 3 (仅 -O3 可现, 因为只有 level>=3 跑本 pass)。
    let touches_base = |idx: usize| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        if let Some(d) = &insts[idx].dest {
            if named.contains(&d.base) {
                v.push(d.base.clone());
            }
        }
        for u in operand_uses(&insts[idx].kind) {
            if let Some(sb) = base_of.get(&u) {
                if named.contains(sb) {
                    v.push(sb.clone());
                }
            }
        }
        for b in inst_def_bases(&insts[idx]) {
            if named.contains(&b) {
                v.push(b);
            }
        }
        v
    };
    {
        let mut last_touch: HashMap<String, usize> = HashMap::new();
        for idx in 0..n {
            let mut seen: HashSet<String> = HashSet::new();
            for base in touches_base(idx) {
                if !seen.insert(base.clone()) {
                    continue;
                }
                if let Some(&prev) = last_touch.get(&base) {
                    add_edge(prev, idx);
                }
                last_touch.insert(base, idx);
            }
        }
    }

    // 保持内存读不移动到更早的内存操作之前；它们仍可向下跨越
    // 后续非 memory asm，这正是该调度器需要释放的场景。
    for (idx, inst) in insts.iter().enumerate() {
        if inst_reads_memory(&inst.kind) {
            for (before, prev) in insts[..idx].iter().enumerate() {
                if inst_is_memory_ordered(&prev.kind) {
                    add_edge(before, idx);
                }
            }
        }
    }

    // 无别名分析时，内存操作保持源码顺序。inline asm 只有在包含
    // `memory` clobber 或显式内存约束时才进入该链，因此非 memory
    // asm 不是全局内存屏障。
    let mut last_memory: Option<usize> = None;
    for (idx, inst) in insts.iter().enumerate() {
        if inst_is_memory_ordered(&inst.kind) {
            if let Some(prev) = last_memory {
                add_edge(prev, idx);
            }
            last_memory = Some(idx);
        }
    }

    // 保留可观察副作用和 volatile-like asm 之间的相对顺序。
    // 这仍允许独立值计算围绕 asm 移动。
    let mut last_side_effect: Option<usize> = None;
    for (idx, inst) in insts.iter().enumerate() {
        if inst_is_side_effecting(&inst.kind) {
            if let Some(prev) = last_side_effect {
                add_edge(prev, idx);
            }
            last_side_effect = Some(idx);
        }
    }

    // 非 asm 副作用维持为完整本地屏障，匹配此前对 call/store 的
    // 保守行为。inline asm 则只在 def-use 或内存规则要求时作为屏障。
    for (idx, inst) in insts.iter().enumerate() {
        if inst_is_full_barrier(&inst.kind) {
            for before in 0..idx {
                add_edge(before, idx);
            }
            for after in idx + 1..n {
                add_edge(idx, after);
            }
        }
    }

    let mut selected = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for _ in 0..n {
        let next = (0..n)
            .filter(|&i| !selected[i] && indeg[i] == 0)
            .min_by(|&a, &b| schedule_key(&insts[a], a).cmp(&schedule_key(&insts[b], b)))?;
        selected[next] = true;
        order.push(next);
        for to in succ[next].clone() {
            indeg[to] -= 1;
        }
    }
    Some(order)
}

fn schedule_key(inst: &Inst, idx: usize) -> (u8, String, usize) {
    let class = if inst_is_movable_value(&inst.kind) {
        0
    } else if matches!(inst.kind, InstKind::InlineAsm { .. }) {
        1
    } else {
        2
    };
    let name = inst
        .dest
        .as_ref()
        .map(|d| d.name())
        .unwrap_or_else(|| format!("~{idx:08}"));
    (class, name, idx)
}

fn inst_defs(inst: &Inst) -> Vec<String> {
    let mut defs = Vec::new();
    if let Some(d) = &inst.dest {
        defs.push(d.name());
    }
    if let InstKind::InlineAsm { output_vars, .. } = &inst.kind {
        defs.extend(output_vars.iter().map(|v| v.name()));
    }
    // 用户声明 (`Decl::Var` 整体保留下来的那条指令) 也是定义：它把带初值的对象建起来。
    // 不报出来，调度器就看不见「值消费者依赖它」这条边，能把声明挖到读者之后（实测
    // `v2si v = {..}; unsigned long long b = (unsigned long long)v;` 在 -O3 把声明排到
    // Cast 之后，拿到全 0 的位模式）。
    if let InstKind::PreservedDecl { var, .. } = &inst.kind {
        defs.push(var.name.name.clone());
    }
    defs
}

fn inst_def_bases(inst: &Inst) -> Vec<String> {
    let mut defs = Vec::new();
    if let Some(d) = &inst.dest {
        defs.push(d.base.clone());
    }
    if let InstKind::InlineAsm { output_vars, .. } = &inst.kind {
        defs.extend(output_vars.iter().map(|v| v.base.clone()));
    }
    // 同 `inst_defs`：基名序就是本 IR 的语义序，声明必须是这个链上的一环。
    if let InstKind::PreservedDecl { var, .. } = &inst.kind {
        defs.push(var.name.name.clone());
    }
    defs
}

fn inst_is_movable_value(kind: &InstKind) -> bool {
    // volatile Load 不是"可移动值": 其读取有外部可见副作用,
    // 调度器不能把它当作纯值运算随意重排
    match kind {
        InstKind::Load { is_volatile, .. }
        | InstKind::IndexLoad { is_volatile, .. }
        | InstKind::MemberLoad { is_volatile, .. } => !*is_volatile,
        _ => matches!(
            kind,
            InstKind::Bin { .. }
                | InstKind::Un { .. }
                | InstKind::Cast { .. }
                | InstKind::AddrOf { .. }
                | InstKind::MemberAddr { .. }
                // `IndexAddr` 与 `AddrOf`/`MemberAddr` 同类：纯地址算术，不读不写，
                // 所以可以像普通 Bin 运算一样重排/内联。这里没被编译器点到（本 match
                // 有 `_` 兜底），兜底结果会是「不可移动」——安全但白丢优化。
                | InstKind::IndexAddr { .. }
                | InstKind::Load { .. }
                | InstKind::IndexLoad { .. }
                | InstKind::MemberLoad { .. }
                | InstKind::Select { .. }
        ),
    }
}

fn inst_reads_memory(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Load { .. } | InstKind::IndexLoad { .. } | InstKind::MemberLoad { .. }
    )
}

fn inst_is_full_barrier(kind: &InstKind) -> bool {
    match kind {
        InstKind::Store { .. }
        | InstKind::IndexStore { .. }
        | InstKind::MemberStore { .. }
        | InstKind::Call { .. } => true,
        // volatile asm 作为完整屏障，阻止任何指令跨越
        InstKind::InlineAsm { volatile, .. } => *volatile,
        _ => false,
    }
}

fn inst_is_side_effecting(kind: &InstKind) -> bool {
    match kind {
        InstKind::Store { .. }
        | InstKind::IndexStore { .. }
        | InstKind::MemberStore { .. }
        | InstKind::Call { .. }
        | InstKind::Invoke { .. } => true,
        // 保留原始声明 (复合字面量/初始化列表) 本身就是一次**多字内存写**:
        // 不记进副作用链时, 调度器可以把循环体里的读跨到它前面 —— 实测
        // `v4si a={1,2,3,4}; ... c[i]=a[i]+b[i];` 在 -O3 下把 `a[0]+b[0]` 排到了
        // `a`/`b` 的初始化**之前** (读到未初始化栈槽)。
        InstKind::PreservedDecl { .. } => true,
        // volatile Load 属于外部可见的副作用 (读 MMIO/共享内存), 不可被 DCE 消除
        InstKind::Load { is_volatile, .. }
        | InstKind::IndexLoad { is_volatile, .. }
        | InstKind::MemberLoad { is_volatile, .. } => *is_volatile,
        // 只有 volatile asm 或带 memory clobber 的 asm 才有副作用
        InstKind::InlineAsm {
            volatile,
            clobbers,
            output_constraints,
            ..
        } => {
            *volatile
                || asm_has_memory_clobber(clobbers)
                || asm_constraints_touch_memory(output_constraints)
        },
        _ => false,
    }
}

fn inst_is_memory_ordered(kind: &InstKind) -> bool {
    match kind {
        InstKind::Load { .. }
        | InstKind::Store { .. }
        | InstKind::IndexLoad { .. }
        | InstKind::IndexStore { .. }
        | InstKind::MemberLoad { .. }
        | InstKind::MemberStore { .. }
        | InstKind::Call { .. }
        | InstKind::Invoke { .. } => true,
        // 同 `inst_is_side_effecting`: 带初始化式的声明是一次内存写, 不可越序。
        InstKind::PreservedDecl { .. } => true,
        InstKind::InlineAsm {
            clobbers,
            input_constraints,
            output_constraints,
            ..
        } => {
            asm_has_memory_clobber(clobbers)
                || asm_constraints_touch_memory(input_constraints)
                || asm_constraints_touch_memory(output_constraints)
        },
        _ => false,
    }
}

fn asm_has_memory_clobber(clobbers: &[String]) -> bool {
    clobbers.iter().any(|c| c.trim_matches('"') == "memory")
}

fn asm_constraints_touch_memory(constraints: &[String]) -> bool {
    constraints.iter().any(|c| c.contains('m'))
}


// =====================================================================
// P8 循环分块 / P9 循环交换 —— 已删除 (伪实现会静默误编译)
// =====================================================================
//
// 两个旧 pass 都只按 **BB 名字** 猜测循环形态，且都不产生真实变换：
//
//   * `loop_tiling`：“循环体 <= 4 条指令时，把所有纯运算指令**复制一份追加**到
//     末尾，模拟 2x unroll”。这不是分块也不是展开 (迭代次数没改、跳长没改)，而且
//     **重复使用同一个 SSA dest** —— 直接破坏单赋值不变式。SSA→AST 重建按基名
//     塌缩后会**重算两遍**：实测 `int s=0,m=3; for(i<n){s+=m*i; m=m+1;}` 在 -O3
//     得 38 (应 32)，因为副本 `_tbin = m*i` 被排到了 `m = m+1` **之后**重执行。
//   * `loop_interchange`：按名字含 `.i.` / `.j.` 的块在 `block_order` 里**互换位置**，
//     并往 entry 写一条 `__dcc_loop_interchanged = 1` 的假 hint。它不改任何
// terminator / phi / 边界，所以“交换”对执行顺序毫无作用；而 `block_order` 是重建
// 与遍历的创建序真源，改动它只会扰动产物字节。另一点：`i_bbs` 的判据
// `n.contains("for.body") && !n.contains(".j.")` 会把任意单层循环也归为“外层”，
// 于是几乎每个含循环的函数都被改一次 `block_order`。
//
// 真正的分块/交换必须同时重写 CFG、phi 与边界 (且需先修好下游对多层嵌套 +
// 变量边界形态的误编译，见 `docs/ssa-pass-upgrade-plan.md` 的「未关闭缺陷」)，
// 保留“看起来在做”的伪实现只会把错值交给后端，故删。追踪项: P9/P10。

// =====================================================================
// P10. TCO (Tail Call Optimization)
// =====================================================================

/// 检测函数末尾 `return f(...)` 形式: 如果 `f` 名字与当前函数相同,
/// 将尾递归调用转换为跳转到函数入口 BB (用 goto start 代替 call + ret)。
/// 具体操作: 将 Call 指令替换为参数赋值 + Jump 到 entry BB。
pub fn tail_call_optimization(func: &mut Function) -> bool {
    if func.blocks.is_empty() {
        return false;
    }
    let entry_name = func.entry.clone();
    // 检查最后一个 BB 是否含尾递归调用
    let exit_bb_name = match func.block_order.last() {
        Some(n) => n.clone(),
        None => return false,
    };
    let bb = match func.blocks.get(&exit_bb_name) {
        Some(b) => b,
        None => return false,
    };
    // 找最后一个指令是否为 self-call
    let mut self_call_idx = None;
    for (i, inst) in bb.insts.iter().enumerate() {
        if let InstKind::Call {
            callee: Operand::FnAddr(n, _) | Operand::SymAddr(n, _),
            ..
        } = &inst.kind
        {
            if n == &func.name {
                self_call_idx = Some(i);
            }
        }
    }
    let call_idx = match self_call_idx {
        Some(i) => i,
        None => return false,
    };
    // 将 Call 指令的参数提取出来, 然后替换为 Jump 到 entry
    let bb = match func.blocks.get_mut(&exit_bb_name) {
        Some(b) => b,
        None => return false,
    };
    // 保留 Call 之前的指令 (参数计算), 删除 Call 本身
    let call_inst = bb.insts.remove(call_idx);
    // 将 terminator 替换为 Jump 到 entry (尾递归)
    bb.term = Terminator::Jump(entry_name.clone());
    if let Some(bb) = func.blocks.get_mut(&exit_bb_name) {
        if !bb.succs.contains(&entry_name) {
            bb.succs.push(entry_name);
        }
    }
    // 记录尾递归标记
    func.is_noreturn = false;
    let _ = call_inst;
    true
}

// =====================================================================
// P11. 函数属性 (cold / hot / noreturn) —— 不在本层做
// =====================================================================
//
// 旧 P11 `function_attribute_propagation` 在 entry BB 顶端伪造三条
// `Store{e: SymAddr("__dcc_section_cold"), src: 1}` / `__dcc_section_hot` /
// `__dcc_noreturn_hint` 当「段属性 hint」。它们不是 hint, 而是**真实指令**:
// `__dcc_section_*` 不在重建期的 hint 丢弃名单里, 于是被当普通内存写送到后端,
// 而后端认不出这个并不存在的符号, 按「未声明名字」回退成**本帧栈槽** ——
// `-O2` 实测 `__attribute__((cold)) int cold_path(int x){return x+1;}` 发出
// `movslq -0x48(%r13),%rax; push; mov $1,%rax; pop %rcx; mov %rax,(%rcx)`,
// 即向一个未初始化槽里存的地址写 1 → 运行期 SEGV (夹具 function_attr 直接崩)。
//
// 热/冷段归属的真源是 `FuncDecl::is_hot` / `is_cold` / `is_noreturn` 这三个 AST
// 属性 (后端 `codegen.rs` 直接读它们选 section), 而 `run_ssa_passes` 只回写
// `f.body` —— SSA 层的属性位改动本来就不会回到 AST。因此正确做法不是「把 hint
// 发对」, 而是删掉这个 pass: 它既改不动真源, 又在产物里造了一条会崩的写。

// =====================================================================
// P12. 寄存器分配 —— 占位实现已删, 真正的分配不属本层
// =====================================================================
//
// 旧 P12 在 entry BB 塑一条 `Store{e: SymAddr("__dcc_regalloc_done"), src: <统计量>}`
// 并 `return true`。三件事同时不成立:
//   1. 后端从不读这个符号 (`grep -rn __dcc_ src/codegen/` 只有 `__dcc_trapv_overflow`
//      / `__dcc_tls_base` / `__dcc_x87_fmod` 这三个无关内建), 重建时又被
//      `is_pass_hint_inst` 丢掉 ⇒ 对机器码零影响;
//   2. 只插了一条 hint 却报 `changed = true` ⇒ 任何拿返回值驱动定点迭代的调度器
//      都会被它骗成无限循环;
//   3. 方案要求的「重建 AST 时每个 `Ident` 携带物理寄存器编号、codegen 不再分配」
//      在当前架构下不是 pass 级改动: `ast::Ident` 没有寄存器字段, 而三个后端的变量
//      模型全部是「栈槽 + 固定累加器」(x86_64 `var_map` + `%r13+disp` + `%rax`;
//      aarch64 `x0/w0`; riscv64 `a0/a1`), `ArchRegPool` 只服务 inline asm 操作数绑定。
//      要真做必须先有「寄存器直用 + 溢出物化」的发射后端 —— 后端级重写, 需单独立项
//      (含与 golden baseline / 产物可复现门禁的逐字节对撞), 不属本方案范围。
//
// 删而非留的理由: 不影响产物的 pass 不叫「已完成」, 叫死代码; 而它的区间计算与扫描
// 算法已在 git 历史里, 后端重写时可直接取回。本文件的头注释同步修正了 P12 的条目。

// =====================================================================
// P12b. UBSAN IR 层插桩
// =====================================================================

/// UBSAN IR 层插桩: 在优化后的 SSA IR 上扫描可能产生未定义行为的
/// 指令, 插入运行时检查 guard。与 AST 层 sanitizer 互补:
/// - AST 层在 SSA 构造前插桩, 优化可能消除某些检查
/// - IR 层在优化后插桩, 能捕获优化新建的操作和消除后残留的 UB
///
/// 检查类型:
///   - 有符号除法/取余: 除零 + 有符号溢出 (INT_MIN / -1)
///   - 移位: 移位量 < 0 或 >= 操作数位宽
///   - 有符号乘/加/减: 溢出检查
///   - 取反: 有符号溢出 (negate INT_MIN)
///
/// 插桩方式: 在每条可能 UB 的指令后插入 Store hint:
///   `__dcc_ubsan_<kind>(op_info)` 其中 op_info 编码操作数类型和位置
///   SSA→AST 重建时这些 hint 被识别并转换为 `__ubsan_handle_*` 调用。
pub fn ubsan_ir_instrument(func: &mut Function) -> bool {
    let mut changed = false;
    let span = func.span.clone();
    // 收集所有 BB 名称, 避免借用冲突
    let bb_names: Vec<String> = func.block_order.clone();
    for bb_name in &bb_names {
        let bb = match func.blocks.get(bb_name) {
            Some(b) => b,
            None => continue,
        };
        let mut new_insts: Vec<Inst> = Vec::new();
        for inst in &bb.insts {
            new_insts.push(inst.clone());
            // 检查当前指令是否需要 UBSAN guard
            if let Some(d) = &inst.dest {
                if let Some(guard) = gen_ubsan_guard(&inst.kind, d, &span) {
                    new_insts.push(guard);
                    changed = true;
                }
            }
        }
        if changed {
            if let Some(bb_mut) = func.blocks.get_mut(bb_name) {
                bb_mut.insts = new_insts;
            }
        }
    }
    changed
}

/// 判断类型是否为有符号整数 (有符号溢出才是 UB, 无符号回绕是定义行为).
fn is_signed_int_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Char
            | Type::SChar
            | Type::Short
            | Type::Int
            | Type::Long
            | Type::LongLong
            | Type::Int128
    )
}

/// 为单条 SSA 指令生成 UBSAN guard (如果该指令可能 UB).
fn gen_ubsan_guard(kind: &InstKind, dest: &SsaVar, span: &Span) -> Option<Inst> {
    let dest_signed = is_signed_int_ty(&dest.ty);
    match kind {
        InstKind::Bin { op, l: _, r } => {
            let check_kind = match op {
                // 除法/取余: 除零 + 有符号 INT_MIN / -1
                BinOp::Div | BinOp::Mod => {
                    // 检查右操作数是否为 0
                    if is_const_nonzero(r) {
                        return None; // 编译期已知非零, 无需检查
                    }
                    Some("divrem_overflow")
                },
                // 移位: 移位量 < 0 或 >= 位宽
                BinOp::Shl | BinOp::Shr => {
                    if is_const_valid_shift(r) {
                        return None;
                    }
                    Some("shift_out_of_bounds")
                },
                // 有符号加法/减法/乘法: 溢出 (无符号回绕是定义行为, 不检查)
                BinOp::Add if dest_signed => Some("add_overflow"),
                BinOp::Sub if dest_signed => Some("sub_overflow"),
                BinOp::Mul if dest_signed => Some("mul_overflow"),
                _ => None,
            };
            if let Some(kind_str) = check_kind {
                // 编码: kind_hash 用于运行时选择 handler
                let kind_hash = ubsan_kind_hash(kind_str);
                return Some(Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: Operand::SymAddr(
                            format!("__dcc_ubsan_{}", kind_str),
                            Type::Ptr(Box::new(QualType::new(Type::Void))),
                        ),
                        src: Operand::Int(kind_hash),
                        is_volatile: false,
                    },
                    span: span.clone(),
                });
            }
            None
        },
        InstKind::Un { op, e: _ } => {
            // 取反: 仅对有符号整数, INT_MIN 取反溢出
            if matches!(op, UnOp::Neg) && dest_signed {
                return Some(Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: Operand::SymAddr(
                            "__dcc_ubsan_negate_overflow".to_string(),
                            Type::Ptr(Box::new(QualType::new(Type::Void))),
                        ),
                        src: Operand::Int(ubsan_kind_hash("negate_overflow")),
                        is_volatile: false,
                    },
                    span: span.clone(),
                });
            }
            None
        },
        _ => None,
    }
}

/// 检查 Operand 是否为编译期已知的非零常量。
fn is_const_nonzero(op: &Operand) -> bool {
    matches!(op, Operand::Int(v) if *v != 0)
}

/// 检查移位操作数是否为编译期已知的有效移位量 (0 <= shift < 64)。
fn is_const_valid_shift(op: &Operand) -> bool {
    matches!(op, Operand::Int(v) if *v >= 0 && *v < 64)
}

/// FNV-1a hash 用于将 check kind 名称映射到唯一整数 ID。
fn ubsan_kind_hash(kind: &str) -> i128 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in kind.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i128
}

// =====================================================================
// P14. Speculative execution
// =====================================================================

/// 投机执行: 把「分支条件真正需要、且提前求值不新增可观察行为」的纯计算提到 BB 顶端,
/// 让分支尽早算出 (在本块内把这条链上提, 后端便能尽早发分支)。
///
/// 旧实现是把块内指令按 dest 名字**字典序排序**, 注释写着「无依赖冲突」—— 这是错的:
/// 字典序与数据依赖没有任何关系。`s = s + (t + 0)` 会排到 `t = gv*k + C` 之前 (s < t),
/// SSA→AST 重建后 `t` 的使用跑在赋值之前 —— 实测 -O3 下 `tri(3)` 由 2754876 变 12。
/// 因此本版只允许沿**依赖闭包**上提, 并拒绝把可能陷隟的运算 (除/模) 提到
/// 除数就绪之前。
pub fn speculative_execution(func: &mut Function) -> bool {
    let mut changed = false;
    for b in func.blocks.values_mut() {
        if b.insts.len() < 2 {
            continue;
        }
        // 保守守卫 (与旧版一致): 块内有副作用指令 (Call/Store/volatile 读/asm) 时,
        // 提前的纯计算可能与副作用调序。
        let has_side = b.insts.iter().any(|i| {
            // volatile Load 也有外部可见副作用, 不可被投机重排
            inst_is_side_effecting(&i.kind) || matches!(i.kind, InstKind::InlineAsm { .. })
        });
        if has_side {
            continue;
        }
        // 后向切片: 从终结符真正用到的名字出发, 逆序收集「定义这些名字的指令」,
        // 并把它们的操作数加进需求集。逆序遍历保证同一个名字只取**最近**的定义。
        let mut need: HashSet<String> = HashSet::new();
        match &b.term {
            Terminator::Ret { val: Some(v) } => need.extend(operand_uses_op(v)),
            Terminator::Branch { cond, .. } => need.extend(operand_uses_op(cond)),
            Terminator::SwitchTable { idx, .. } => need.extend(operand_uses_op(idx)),
            _ => {},
        }
        if need.is_empty() {
            continue;
        }
        let mut slice: Vec<usize> = Vec::new();
        let mut blocked = false;
        for idx in (0..b.insts.len()).rev() {
            let inst = &b.insts[idx];
            let defines_need = inst.dest.as_ref().is_some_and(|d| need.contains(&d.name()));
            if !defines_need {
                continue;
            }
            // 除/模可能陷隟: 上提后会跳过原本保护它的控制流, 整个块放弃。
            if may_trap(&inst.kind) {
                blocked = true;
                break;
            }
            need.extend(operand_uses(&inst.kind));
            slice.push(idx);
        }
        if blocked || slice.is_empty() {
            continue;
        }
        slice.sort_unstable();
        let picked: HashSet<usize> = slice.iter().copied().collect();
        // 关键屏障: 本 IR 的重建是**按基名塌缩成可变量**的 —— `y_1/y_2/y_3` 在 SSA 里
        // 是互不相关的名字, 重建后都是变量 `y`, 它们的**相对顺序就是程序语义**。
        // 因此上提集合不得跨过一个「写同一基名」或「读了对方所写基名」的指令。
        // (同一约束在 P5 `schedule_block` 里由 `inst_def_bases` 边建模, 本 pass 同样补上。)
        if !slice_is_movable(&b.insts, &picked) {
            continue;
        }
        if picked.iter().max().copied().unwrap_or(0) < picked.len() {
            continue;
        }
        let orig = std::mem::take(&mut b.insts);
        let mut next: Vec<Inst> = Vec::with_capacity(orig.len());
        for (idx, inst) in orig.iter().enumerate() {
            if picked.contains(&idx) {
                next.push(inst.clone());
            }
        }
        for (idx, inst) in orig.iter().enumerate() {
            if !picked.contains(&idx) {
                next.push(inst.clone());
            }
        }
        changed = true;
        b.insts = next;
    }
    changed
}

/// 切片能否整体提到块首: 对每个被上提的 `i` 与它跨过每个留在原地的 `j (j<i)` 检查
/// 三条不得跨越的关系。
fn slice_is_movable(insts: &[Inst], picked: &HashSet<usize>) -> bool {
    let base_of = |k: usize| -> Option<String> { insts[k].dest.as_ref().map(|d| d.base.clone()) };
    for &i in picked {
        for j in 0..i {
            if picked.contains(&j) {
                continue;
            }
            // 1. 两者写同一个基名 ⇒ 上提会颠倒对同一变量的两次赋值。
            if let (Some(bi), Some(bj)) = (base_of(i), base_of(j)) {
                if bi == bj {
                    return false;
                }
                // 2. 上提项读了被跨过指令所写的基名 ⇒ 拿到的会是旧值。
                if operand_uses(&insts[i].kind).iter().any(|u| u == &bj) {
                    return false;
                }
            }
            // 3. 上提项的 dest 被被跨过指令以**同名 SSA** 读写 (版本级冲突)。
            if let Some(d) = &insts[i].dest {
                let dn = d.name();
                if operand_uses(&insts[j].kind).iter().any(|u| *u == dn) {
                    return false;
                }
            }
        }
    }
    true
}

/// 该运算是否可能陷隟: 整数除零在 C 里是 UB, 但 dcc 会发 trap / UBSAN 守卫,
/// 那是可观察行为 ⇒ 除数不是已知非零常量时, 不得把这条除法上提。
fn may_trap(kind: &InstKind) -> bool {
    match kind {
        InstKind::Bin { op: BinOp::Div | BinOp::Mod, r, .. } => {
            !matches!(r, Operand::Int(v) if *v != 0)
        },
        _ => false,
    }
}

// =====================================================================
// P15. SCCP (Sparse Conditional Constant Propagation)
// =====================================================================

/// 简化版 SCCP: 单函数内, 对每个 SSA 值维护 lattice { Undef, Const(v), Top }。
/// 工作流:
///   1. entry BB 所有指令标 Const (若可计算)
///   2. 沿 CFG 流: 若所有到达值都是同一个 Const, 接受; 否则 Top
///   3. 替换 SSA 名为 Const, 删除冗余运算
///
/// 返回是否改动了 IR。
pub fn sparse_conditional_constant_propagation(func: &mut Function) -> bool {
    // 1. 收集所有 SSA 名 + 常量折叠
    let mut consts: HashMap<String, i128> = HashMap::new();
    let mut changed = true;
    let mut iter = 0;
    // 访问序必须固定: 本 pass 在同一遍里既读 `consts` 又写 `consts`, 而迭代上限是
    // 16 轮 —— 预算耗尽时拿到的是"部分不动点", 其内容依赖于访问序。直接用
    // `blocks.values()` (HashMap 随机序) 就会让同一函数在不同进程里折出不同的
    // 常量集, 进而泄进产物字节。`ordered_block_names` 不改变收敛结果 (真正收敛到
    // 不动点时与序无关), 只是让"未收敛"情形也保持可复现。
    let sweep_order = ordered_block_names(func);
    // 哪些名字可以被当常量传递? 两类必须排除, 它们同属一个根因: 本 IR 的 SSA→AST
    // 重建是**按基名塌缩成可变量**的 (`s_1`/`s_2` 重建后都是变量 `s`), 所以
    // 「这个名字在这个点一定是这个值」只在基名全局唯一赋值时成立。
    //   1. 参与任何 phi 的名字 (包括 phi dest) —— 它们是汇合点上的值;
    //   2. 基名在本函数内有 **多个版本** 的名字 —— 典型即循环携带变量
    //      (`int s=0; while(n>0){ s+=n; ... }` 造出 s_1/s_2)。
    // 不排除会怎样: `s_1 = 0` 入表后, 循环体里的 `s_2 = s_1 + n` 被替换成
    // `s = 0 + n` —— 累加变成赋置, 实测 `sum_down(3)` 返回垃圾值。
    // 真正的 SCCP 应在 phi 上做格 meet; 在那之前, 保守判据就是上面两条
    // (只损失折叠精度, 不损失正确性)。
    let phi_locked = non_propagatable_names(func);
    while changed && iter < 16 {
        changed = false;
        iter += 1;
        for bb_name in &sweep_order {
            let Some(b) = func.blocks.get(bb_name) else {
                continue;
            };
            for inst in &b.insts {
                let v = match &inst.kind {
                    InstKind::Bin { op, l, r } => {
                        let lv = operand_int(l, &consts);
                        let rv = operand_int(r, &consts);
                        match (lv, rv) {
                            (Some(a), Some(b)) => {
                                // Mask operands to dest type width before
                                // evaluating, so that e.g. unsigned div sees
                                // the correct positive value rather than a
                                // sign-extended i128 negative.
                                if let Some(d) = &inst.dest {
                                    let bits = type_bits(&d.ty);
                                    if let Some(bits) = bits {
                                        if bits < 128 {
                                            let mask = if bits == 64 {
                                                u64::MAX as i128
                                            } else {
                                                (1i128 << bits) - 1
                                            };
                                            let a = a & mask;
                                            let b = b & mask;
                                            eval_binop(*op, a, b)
                                        } else {
                                            eval_binop(*op, a, b)
                                        }
                                    } else {
                                        eval_binop(*op, a, b)
                                    }
                                } else {
                                    eval_binop(*op, a, b)
                                }
                            },
                            _ => None,
                        }
                    },
                    InstKind::Un { op, e } => {
                        operand_int(e, &consts).and_then(|v| eval_unop(*op, v))
                    },
                    InstKind::Cast { e, .. } => operand_int(e, &consts),
                    _ => None,
                };
                if let Some(d) = &inst.dest {
                    if let Some(cv) = v {
                        // phi 参与的名字不得入常量表 (见上方 `phi_locked` 的根因说明)。
                        if phi_locked.contains(&d.name()) {
                            continue;
                        }
                        // 本表的值是无类型的 i128，只有**整数类**变量能代表它；浮点/
                        // long double 名字一旦入表，后面的替换就把 ABI 类型类换掉了
                        // (见 `replace_op` 的根因说明)。
                        if type_bits(&d.ty).is_none() {
                            continue;
                        }
                        // Mask constant to destination type width to avoid
                        // i128 sign-extension issues (e.g. ~0U must be
                        // 0xFFFFFFFF, not -1 in i128).
                        let cv = mask_to_type(cv, &d.ty);
                        if consts.get(&d.name()) != Some(&cv) {
                            consts.insert(d.name(), cv);
                            changed = true;
                        }
                    }
                }
            }
            if let Terminator::Branch { cond, .. } = &b.term {
                if let Some(v) = operand_int(cond, &consts) {
                    if consts.get(&format!("__branch__{}", b.name)) != Some(&v) {
                        consts.insert(format!("__branch__{}", b.name), v);
                        changed = true;
                    }
                }
            }
            // SwitchTable: 如果 idx 是常量, 记录以便后续解析
            if let Terminator::SwitchTable { idx, .. } = &b.term {
                if let Some(v) = operand_int(idx, &consts) {
                    if consts.get(&format!("__sw__{}", b.name)) != Some(&v) {
                        consts.insert(format!("__sw__{}", b.name), v);
                        changed = true;
                    }
                }
            }
        }
    }
    // 2. 用 consts 替换 operand
    let mut modified = false;
    for b in func.blocks.values_mut() {
        for inst in &mut b.insts {
            replace_op_in_inst(&mut inst.kind, &consts);
        }
        replace_op_in_term(&mut b.term, &consts);
        // SwitchTable 常量解析: 如果 idx 是已知常量, 替换为 Jump
        if let Terminator::SwitchTable {
            idx,
            values,
            targets,
            default,
        } = &b.term
        {
            let idx_val = match idx {
                Operand::Int(v) => Some(*v),
                Operand::Var(sv) => consts.get(&sv.name()).copied(),
                _ => None,
            };
            if let Some(v) = idx_val {
                let target = values
                    .iter()
                    .position(|&cv| cv == v)
                    .map(|i| targets[i].clone())
                    .unwrap_or_else(|| default.clone());
                b.succs = vec![target.clone()];
                b.term = Terminator::Jump(target);
                modified = true;
            }
        }
    }
    // 3. 收集所有被引用的 SSA 变量名 (操作数 + terminator)
    let mut used: HashSet<String> = HashSet::new();
    for b in func.blocks.values() {
        for inst in &b.insts {
            for u in operand_uses(&inst.kind) {
                used.insert(u);
            }
        }
        match &b.term {
            Terminator::Ret { val: Some(o) } => {
                for u in operand_uses_op(o) {
                    used.insert(u);
                }
            },
            Terminator::Branch { cond, .. } => {
                for u in operand_uses_op(cond) {
                    used.insert(u);
                }
            },
            Terminator::SwitchTable { idx, .. } => {
                for u in operand_uses_op(idx) {
                    used.insert(u);
                }
            },
            _ => {},
        }
        for p in &b.phis {
            for (v, _) in &p.incoming {
                used.insert(v.name());
            }
        }
    }
    // 4. 删除冗余纯运算: 仅当 dest 不再被引用时才可安全删除
    for b in func.blocks.values_mut() {
        let before = b.insts.len();
        b.insts.retain(|i| {
            if let Some(d) = &i.dest {
                if consts.contains_key(&d.name()) && !used.contains(&d.name()) {
                    return false; // 常量已传播且无引用, 指令多余
                }
            }
            true
        });
        if b.insts.len() != before {
            modified = true;
        }
    }
    modified
}

/// 不允许进常量表的名字集合 (根因见 `sparse_conditional_constant_propagation` 内的注释)。
fn non_propagatable_names(func: &Function) -> HashSet<String> {
    let mut locked: HashSet<String> = HashSet::new();
    // 基名 → 出现过的不同 SSA 版本数。
    let mut versions: HashMap<String, HashSet<String>> = HashMap::new();
    for bb in func.blocks.values() {
        for phi in &bb.phis {
            locked.insert(phi.dest.name());
            for (v, _) in &phi.incoming {
                locked.insert(v.name());
            }
            versions
                .entry(phi.dest.base.clone())
                .or_default()
                .insert(phi.dest.name());
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                versions.entry(d.base.clone()).or_default().insert(d.name());
            }
        }
    }
    for p in &func.params {
        versions.entry(p.base.clone()).or_default().insert(p.name());
    }
    // 多版本基名下的**全部**版本都不可传播: 只要基名是个可变量, 任何一次替换都可能
    // 发生在它已被后续赋值改写之后。
    for (_base, vs) in versions {
        if vs.len() > 1 {
            locked.extend(vs);
        }
    }
    locked
}

fn operand_int(o: &Operand, consts: &HashMap<String, i128>) -> Option<i128> {
    match o {
        Operand::Int(v) => Some(*v),
        Operand::Var(v) => consts.get(&v.name()).copied(),
        _ => None,
    }
}

fn replace_op_in_inst(k: &mut InstKind, consts: &HashMap<String, i128>) {
    match k {
        InstKind::Bin { l, r, .. } => {
            replace_op(l, consts);
            replace_op(r, consts);
        },
        InstKind::Un { e, .. } => replace_op(e, consts),
        InstKind::Cast { e, .. } => replace_op(e, consts),
        InstKind::Load { e, .. } => replace_op(e, consts),
        InstKind::Store { e, src, .. } => {
            replace_op(e, consts);
            replace_op(src, consts);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            replace_op(arr, consts);
            replace_op(idx, consts);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            replace_op(arr, consts);
            replace_op(idx, consts);
            replace_op(src, consts);
        },
        InstKind::Call { callee, args, .. } => {
            replace_op(callee, consts);
            for a in args {
                replace_op(a, consts);
            }
        },
        InstKind::MemberLoad { obj, .. } => replace_op(obj, consts),
        InstKind::MemberStore { obj, src, .. } => {
            replace_op(obj, consts);
            replace_op(src, consts);
        },
        InstKind::Select { cond, then_, else_ } => {
            replace_op(cond, consts);
            replace_op(then_, consts);
            replace_op(else_, consts);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                replace_op(input, consts);
            }
        },
        _ => {},
    }
}

fn replace_op_in_term(t: &mut Terminator, consts: &HashMap<String, i128>) {
    match t {
        Terminator::Ret { val: Some(v) } => replace_op(v, consts),
        Terminator::Branch { cond, .. } => replace_op(cond, consts),
        Terminator::SwitchTable { idx, .. } => replace_op(idx, consts),
        _ => {},
    }
}

/// 把一个已被判为常量的名字替成 `Operand::Int`。
///
/// 为什么必须看变量自己的 SSA 类型：`consts` 是无类型的 i128，而 IR 的类型类是有语义的。
/// `InstKind::Cast{e, ..} => operand_int(e, ..)` 会把 `(long double)50` 登成整数 50，
/// 于是变参调用的实参从 `Var(_tcast_1: LongDouble)` 变成 `Int(50)`——数值看着一样，
/// ABI 类却从 MEMORY 换成了 INTEGER：caller 不再预留 16 字节的溢出区，callee 侧的
/// `va_arg(long double)` 全部读错位置（实测四个 va_arg 夹具同时红，退出码就是首个
/// 读错位置的序号）。只在整数类之间做替换，类型类因此保持不变。
fn replace_op(o: &mut Operand, consts: &HashMap<String, i128>) {
    if let Operand::Var(v) = o {
        if type_bits(&v.ty).is_none() {
            return;
        }
        if let Some(c) = consts.get(&v.name()) {
            *o = Operand::Int(*c);
        }
    }
}

/// Return the bit width of an integer type, or None for non-integer types.
fn type_bits(ty: &Type) -> Option<usize> {
    Some(match ty {
        Type::Bool | Type::Char | Type::SChar | Type::UChar => 8,
        Type::Short | Type::UShort => 16,
        Type::Int | Type::UInt => 32,
        Type::Long | Type::ULong => 64,
        Type::LongLong | Type::ULongLong => 64,
        Type::Int128 | Type::UInt128 => 128,
        _ => return None,
    })
}

/// Mask i128 value to the bit width of the given integer type.
/// For unsigned types: zero-extend (mask upper bits).
/// For signed types: mask then sign-extend from type width.
fn mask_to_type(v: i128, ty: &Type) -> i128 {
    let bits = match type_bits(ty) {
        Some(b) if b < 128 => b,
        _ => return v,
    };
    let mask = if bits == 64 {
        u64::MAX as i128
    } else {
        (1i128 << bits) - 1
    };
    let truncated = v & mask;
    let is_signed = matches!(
        ty,
        Type::Char
            | Type::SChar
            | Type::Short
            | Type::Int
            | Type::Long
            | Type::LongLong
            | Type::Int128
    );
    if is_signed {
        let sign_bit = 1i128 << (bits - 1);
        if (truncated & sign_bit) != 0 {
            truncated | !mask
        } else {
            truncated
        }
    } else {
        truncated
    }
}

fn eval_binop(op: BinOp, a: i128, b: i128) -> Option<i128> {
    Some(match op {
        BinOp::Add => a.checked_add(b)?,
        BinOp::Sub => a.checked_sub(b)?,
        BinOp::Mul => a.checked_mul(b)?,
        BinOp::Div => {
            if b == 0 {
                return None;
            } else {
                a / b
            }
        },
        BinOp::Mod => {
            if b == 0 {
                return None;
            } else {
                a % b
            }
        },
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::Shl => a << (b as u32),
        BinOp::Shr => a >> (b as u32),
        _ => return None,
    })
}

fn eval_unop(op: UnOp, v: i128) -> Option<i128> {
    Some(match op {
        UnOp::Neg => v.checked_neg()?,
        UnOp::Not => {
            if v == 0 {
                1
            } else {
                0
            }
        },
        UnOp::BitNot => !v,
        UnOp::Plus => v,
    })
}

// =====================================================================
// 跨函数 DCE: 在 module 层面做, 由 driver 调用 (不放在 run)
// =====================================================================

/// 跨函数 DCE: 给定函数集合 + 已知的入口，标记可达函数，
/// 删除不可达的 internal static 函数 (FuncDecl)。
pub fn cross_function_dce(funcs: &mut Vec<FuncDecl>, keep_extern: bool) -> bool {
    cross_function_dce_with_roots(funcs, keep_extern, &[])
}

pub fn cross_function_dce_with_roots(
    funcs: &mut Vec<FuncDecl>,
    keep_extern: bool,
    extra_roots: &[String],
) -> bool {
    let func_names: HashSet<String> = funcs.iter().map(|f| f.name.name.clone()).collect();
    let mut callees: HashMap<String, Vec<String>> = HashMap::new();
    let mut address_taken: HashSet<String> = HashSet::new();
    for f in funcs.iter() {
        let (calls, addrs) = collect_func_refs(f, &func_names);
        callees.insert(f.name.name.clone(), calls);
        address_taken.extend(addrs);
    }

    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    for f in funcs.iter() {
        if (keep_extern && !f.is_static)
            || f.name.name == "main"
            || f.is_extern
            || f.is_used_attr
            || f.is_retain
            || f.is_constructor
            || f.is_destructor
            || address_taken.contains(&f.name.name)
        {
            stack.push(f.name.name.clone());
        }
    }

    for root in extra_roots {
        if func_names.contains(root) {
            stack.push(root.clone());
        }
    }

    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some(cs) = callees.get(&name) {
            for c in cs {
                if !reachable.contains(c) {
                    stack.push(c.clone());
                }
            }
        }
    }

    let before = funcs.len();
    funcs.retain(|f| {
        if !f.is_static {
            return true;
        }
        if f.is_used_attr || f.is_retain || f.is_constructor || f.is_destructor {
            return true;
        }
        reachable.contains(&f.name.name)
    });
    funcs.len() != before
}

pub fn collect_func_refs(
    f: &FuncDecl,
    func_names: &HashSet<String>,
) -> (Vec<String>, HashSet<String>) {
    let body = match &f.body {
        Some(b) => b,
        None => return (Vec::new(), HashSet::new()),
    };
    let mut calls = Vec::new();
    let mut address_taken = HashSet::new();
    walk_block_refs(body, func_names, &mut calls, &mut address_taken);
    (calls, address_taken)
}

fn walk_block_refs(
    b: &crate::ast::Block,
    func_names: &HashSet<String>,
    calls: &mut Vec<String>,
    address_taken: &mut HashSet<String>,
) {
    use crate::ast::{BlockItem, Decl};
    for item in &b.items {
        match item {
            BlockItem::Stmt(s) => walk_stmt_refs(s, func_names, calls, address_taken),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b2) = &f.body {
                    walk_block_refs(b2, func_names, calls, address_taken);
                }
            },
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &v.init {
                    walk_expr_refs(init, func_names, calls, address_taken);
                }
                // `__attribute__((cleanup(f)))` 是一个**调用**，只是不写成 `Expr::Call`。
                // 不记进调用图，DCE 就会把只被 cleanup 用的 static 函数删掉：实测
                // `-O2` 链接报 `undefined reference to 'hit_cleanup'`，而 `-O0/-O1` 正常。
                if let Some(cf) = &v.cleanup_fn {
                    calls.push(cf.clone());
                }
            },
            _ => {},
        }
    }
}

fn walk_stmt_refs(
    s: &Stmt,
    func_names: &HashSet<String>,
    calls: &mut Vec<String>,
    address_taken: &mut HashSet<String>,
) {
    match s {
        Stmt::Return { val: Some(e), .. } => walk_expr_refs(e, func_names, calls, address_taken),
        Stmt::Return { val: None, .. } => {},
        Stmt::Expr { e, .. } => walk_expr_refs(e, func_names, calls, address_taken),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            walk_expr_refs(cond, func_names, calls, address_taken);
            walk_stmt_refs(then_, func_names, calls, address_taken);
            if let Some(e) = else_ {
                walk_stmt_refs(e, func_names, calls, address_taken);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            walk_expr_refs(cond, func_names, calls, address_taken);
            walk_stmt_refs(body, func_names, calls, address_taken);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                walk_stmt_refs(i, func_names, calls, address_taken);
            }
            if let Some(c) = cond {
                walk_expr_refs(c, func_names, calls, address_taken);
            }
            if let Some(s) = step {
                walk_expr_refs(s, func_names, calls, address_taken);
            }
            walk_stmt_refs(body, func_names, calls, address_taken);
        },
        Stmt::Block { b, .. } => walk_block_refs(b, func_names, calls, address_taken),
        Stmt::Switch { expr, cases, .. } => {
            walk_expr_refs(expr, func_names, calls, address_taken);
            for c in cases {
                walk_block_refs(&c.body, func_names, calls, address_taken);
            }
        },
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                walk_expr_refs(e, func_names, calls, address_taken);
            }
        },
        Stmt::Label { stmt, .. } => {
            walk_stmt_refs(stmt, func_names, calls, address_taken);
        },
        Stmt::GotoIndirect { expr, .. } => {
            walk_expr_refs(expr, func_names, calls, address_taken);
        },
        // C++ 零成本异常: 被保护调用里的被调函数必须被计入 (否则只在这些调用
        // 里出现的函数会被误判为未被调用), 落地垫槽位的取址同样要计入。
        Stmt::Invoke { call, .. } => {
            walk_stmt_refs(call, func_names, calls, address_taken);
        },
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            walk_expr_refs(object, func_names, calls, address_taken);
            walk_expr_refs(selector, func_names, calls, address_taken);
        },
        _ => {},
    }
}

fn walk_init_refs(
    init: &crate::ast::InitItem,
    func_names: &HashSet<String>,
    calls: &mut Vec<String>,
    address_taken: &mut HashSet<String>,
) {
    match init {
        crate::ast::InitItem::Expr(e) => walk_expr_refs(e, func_names, calls, address_taken),
        crate::ast::InitItem::Designated { designator, value } => {
            walk_designator_refs(designator, func_names, calls, address_taken);
            walk_expr_refs(value, func_names, calls, address_taken);
        },
    }
}

fn walk_designator_refs(
    designator: &crate::ast::Designator,
    func_names: &HashSet<String>,
    calls: &mut Vec<String>,
    address_taken: &mut HashSet<String>,
) {
    match designator {
        crate::ast::Designator::Index(e) => walk_expr_refs(e, func_names, calls, address_taken),
        crate::ast::Designator::Range(l, r) => {
            walk_expr_refs(l, func_names, calls, address_taken);
            walk_expr_refs(r, func_names, calls, address_taken);
        },
        crate::ast::Designator::Chain(items) => {
            for item in items {
                walk_designator_refs(item, func_names, calls, address_taken);
            }
        },
        crate::ast::Designator::Field(_) => {},
    }
}

fn walk_expr_refs(
    e: &crate::ast::Expr,
    func_names: &HashSet<String>,
    calls: &mut Vec<String>,
    address_taken: &mut HashSet<String>,
) {
    use crate::ast::Expr;
    match e {
        Expr::Ident { name, .. } => {
            if func_names.contains(&name.name) {
                address_taken.insert(name.name.clone());
            }
        },
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                calls.push(name.name.clone());
            } else {
                walk_expr_refs(callee, func_names, calls, address_taken);
            }
            for a in args {
                walk_expr_refs(a, func_names, calls, address_taken);
            }
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            walk_expr_refs(l, func_names, calls, address_taken);
            walk_expr_refs(r, func_names, calls, address_taken);
        },
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Deref { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::VaArg { ap: e, .. } => walk_expr_refs(e, func_names, calls, address_taken),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            walk_expr_refs(cond, func_names, calls, address_taken);
            walk_expr_refs(then_, func_names, calls, address_taken);
            walk_expr_refs(else_, func_names, calls, address_taken);
        },
        Expr::Index { arr, idx, .. } => {
            walk_expr_refs(arr, func_names, calls, address_taken);
            walk_expr_refs(idx, func_names, calls, address_taken);
        },
        Expr::Member { expr, .. } => walk_expr_refs(expr, func_names, calls, address_taken),
        Expr::StmtExpr { block, .. } => walk_block_refs(block, func_names, calls, address_taken),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                walk_init_refs(init, func_names, calls, address_taken);
            }
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                walk_expr_refs(e, func_names, calls, address_taken);
            }
        },
        _ => {},
    }
}

fn operand_uses(k: &InstKind) -> Vec<String> {
    let mut v = Vec::new();
    collect_uses(k, &mut v);
    v
}

/// 检查操作数是否引用 asm 相关符号 (__dcc_asm_* 前缀).
/// 用于指令调度屏障检测: 任何涉及 asm 标记的操作数都视为副作用.
fn operand_refs_asm(op: &Operand) -> bool {
    match op {
        Operand::SymAddr(name, _) => name.starts_with("__dcc_asm_"),
        Operand::Var(v) => v.name().starts_with("__dcc_asm_"),
        _ => false,
    }
}

/// 收集一条指令读到的**全部** SSA 名 —— 调度/投机/LICM/DCE 的依赖判据都走这一份。
///
/// 这里**故意不写 `_ => {}` 兜底**：兜底臂会让新增的 `InstKind` 变体静默变成「无操
/// 数、无依赖」，于是调度器把一个读地址结果的消费者上提到块首、把生产者留在后面 ⇒
/// 块内 use-before-def；重建按块内指令序发射变量 ⇒ 读到未初始化栈槽（-O2/-O3 静默错值，
/// 链式 `&m[i][j]` 尤其致命）。判据只有一份，就不再需要事后把生产者往前搬的补丁。
/// 穷举臂同时保证：将来加变体时编译器会点名到这里。
fn collect_uses(k: &InstKind, out: &mut Vec<String>) {
    match k {
        InstKind::Bin { l, r, .. } => {
            add_op(l, out);
            add_op(r, out);
        },
        InstKind::Un { e, .. } => add_op(e, out),
        InstKind::Cast { e, .. } => add_op(e, out),
        InstKind::Load { e, .. } => add_op(e, out),
        InstKind::Store { e, src, .. } => {
            add_op(e, out);
            add_op(src, out);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            add_op(arr, out);
            add_op(idx, out);
        },
        // `IndexAddr` 与 `IndexLoad` 是同一操作数形状（`arr` 基址 + `idx` 下标）：两者都
        // 真的**读**这两个值，区别只在是否访问内存。漏掉这条臂就是上面那段历史错值的真因。
        InstKind::IndexAddr { arr, idx, .. } => {
            add_op(arr, out);
            add_op(idx, out);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            add_op(arr, out);
            add_op(idx, out);
            add_op(src, out);
        },
        InstKind::Call { callee, args, .. } => {
            add_op(callee, out);
            for a in args {
                add_op(a, out);
            }
        },
        // C++ 零成本 EH：受保护调用的操作数与普通 Call 同义，此前落在兜底臂里。
        InstKind::Invoke { callee, args, .. } => {
            add_op(callee, out);
            for a in args {
                add_op(a, out);
            }
        },
        InstKind::MemberLoad { obj, .. } => add_op(obj, out),
        // `&obj.field`：`obj` 在这里被当**值**（基址指针）读，与 `MemberLoad` 同理。
        InstKind::MemberAddr { obj, .. } => add_op(obj, out),
        InstKind::MemberStore { obj, src, .. } => {
            add_op(obj, out);
            add_op(src, out);
        },
        InstKind::Select { cond, then_, else_ } => {
            add_op(cond, out);
            add_op(then_, out);
            add_op(else_, out);
        },
        InstKind::InlineAsm {
            inputs, outputs, ..
        } => {
            for input in inputs {
                add_op(input, out);
            }
            for output in outputs {
                collect_expr_uses(output, out);
            }
        },
        // `AddrOf { base }` 的 `base` 是**对象名**（重建时直接装回 `Expr::Ident`），不是一
        // 个会流动的值：它读的是那个对象，而对象序由内存定序规则管，不由 def-use 边管。
        InstKind::AddrOf { .. } => {},
        // 原地保留的声明与 landingpad 的 selector 都不读已有 SSA 值（后者自带 dest）。
        InstKind::PreservedDecl { .. } | InstKind::EhLandingPad { .. } => {},
    }
}

fn collect_expr_uses(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Ident { name, .. } => out.push(name.name.clone()),
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::SizeofExpr { e, .. }
        | Expr::AlignofExpr { e, .. }
        | Expr::Deref { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::Real { e, .. }
        | Expr::Imag { e, .. }
        | Expr::VaArg { ap: e, .. } => collect_expr_uses(e, out),
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            collect_expr_uses(l, out);
            collect_expr_uses(r, out);
        },
        Expr::Index { arr, idx, .. } => {
            collect_expr_uses(arr, out);
            collect_expr_uses(idx, out);
        },
        Expr::Member { expr, .. } => collect_expr_uses(expr, out),
        Expr::Call { callee, args, .. } => {
            collect_expr_uses(callee, out);
            for arg in args {
                collect_expr_uses(arg, out);
            }
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_expr_uses(cond, out);
            collect_expr_uses(then_, out);
            collect_expr_uses(else_, out);
        },
        Expr::StmtExpr { block, .. } => {
            for item in &block.items {
                collect_block_item_uses(item, out);
            }
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                collect_init_uses(init, out);
            }
        },
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for expr in outputs.iter().chain(inputs.iter()) {
                collect_expr_uses(expr, out);
            }
        },
        _ => {},
    }
}

fn collect_init_uses(init: &InitItem, out: &mut Vec<String>) {
    match init {
        InitItem::Expr(e) => collect_expr_uses(e, out),
        InitItem::Designated { designator, value } => {
            collect_designator_uses(designator, out);
            collect_expr_uses(value, out);
        },
    }
}

fn collect_designator_uses(designator: &Designator, out: &mut Vec<String>) {
    match designator {
        Designator::Index(e) => collect_expr_uses(e, out),
        Designator::Range(first, last) => {
            collect_expr_uses(first, out);
            collect_expr_uses(last, out);
        },
        Designator::Chain(items) => {
            for item in items {
                collect_designator_uses(item, out);
            }
        },
        Designator::Field(_) => {},
    }
}

fn collect_block_item_uses(item: &BlockItem, out: &mut Vec<String>) {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &v.init {
                collect_expr_uses(init, out);
            }
        },
        BlockItem::Stmt(stmt) => collect_stmt_uses(stmt, out),
        _ => {},
    }
}

fn collect_stmt_uses(stmt: &Stmt, out: &mut Vec<String>) {
    match stmt {
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => collect_expr_uses(e, out),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            collect_expr_uses(cond, out);
            collect_stmt_uses(then_, out);
            if let Some(stmt) = else_ {
                collect_stmt_uses(stmt, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            collect_expr_uses(cond, out);
            collect_stmt_uses(body, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(init) = init {
                collect_stmt_uses(init, out);
            }
            if let Some(cond) = cond {
                collect_expr_uses(cond, out);
            }
            if let Some(step) = step {
                collect_expr_uses(step, out);
            }
            collect_stmt_uses(body, out);
        },
        Stmt::Switch { expr, cases, .. } => {
            collect_expr_uses(expr, out);
            for case in cases {
                if let Some(val) = &case.val {
                    collect_expr_uses(val, out);
                }
                if let Some(range_end) = &case.range_end {
                    collect_expr_uses(range_end, out);
                }
                for item in &case.body.items {
                    collect_block_item_uses(item, out);
                }
            }
        },
        Stmt::Block { b, .. } => {
            for item in &b.items {
                collect_block_item_uses(item, out);
            }
        },
        Stmt::Label { stmt, .. } => collect_stmt_uses(stmt, out),
        Stmt::GotoIndirect { expr, .. } => collect_expr_uses(expr, out),
        // C++ 零成本异常: 受保护调用的被调语句包在 `call` 里, 落地垫序言把 %rax/%rdx
        // 写入两个局部槽。不下降到这里, 只在受保护调用里出现的变量会被当成未使用
        // (错误 DCE / 存活分析漏记)。
        Stmt::Invoke {
            call,
            landing_pad,
            actions,
            ..
        } => {
            collect_stmt_uses(call, out);
            // 落地垫标签与 action 链里的 typeinfo 符号名不是变量引用, 不参与存活。
            let _ = (landing_pad, actions);
        },
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            collect_expr_uses(object, out);
            collect_expr_uses(selector, out);
        },
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for expr in outputs.iter().chain(inputs.iter()) {
                collect_expr_uses(expr, out);
            }
        },
        _ => {},
    }
}

fn add_op(o: &Operand, out: &mut Vec<String>) {
    if let Operand::Var(v) = o {
        out.push(v.name());
    }
}

fn operand_uses_op(o: &Operand) -> Vec<String> {
    let mut v = Vec::new();
    if let Operand::Var(x) = o {
        v.push(x.name());
    }
    v
}

// =====================================================================
// 通用 CFG / 重命名工具 (P17 / P18 / P19 共用)
// =====================================================================

/// 从终结符派生后继。**不得**读 `bb.succs`: `succs`/`preds` 是 SSA 构造时填的缓存,
/// 任何改 terminator 的 pass 之后即为脏 (见 `crate::ssa::cfg::term_succs` 的同一说明)。
/// P19 会改 terminator, 因此所有新 pass 一律走终结符。
fn succs_of(term: &Terminator) -> Vec<String> {
    match term {
        Terminator::Jump(t) => vec![t.clone()],
        Terminator::Branch { t, f, .. } => vec![t.clone(), f.clone()],
        Terminator::SwitchTable {
            targets, default, ..
        } => {
            let mut s: Vec<String> = targets.clone();
            if !s.contains(default) {
                s.push(default.clone());
            }
            s
        },
        Terminator::Ret { .. } | Terminator::Unreachable => vec![],
    }
}

/// 反向重建前驱表 (同样只读终结符)。
fn preds_from_terminators(func: &Function) -> HashMap<String, Vec<String>> {
    let mut preds: HashMap<String, Vec<String>> = HashMap::new();
    for name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&name) else {
            continue;
        };
        for s in succs_of(&bb.term) {
            let e = preds.entry(s).or_default();
            if !e.contains(&name) {
                e.push(name.clone());
            }
        }
    }
    preds
}

/// 从 entry 出发的可达块, 按 **RPO** (逆后序) 给出。
///
/// 为什么需要 RPO 而不是 `block_order`: 值编号与死代码判定都要求「处理一个块时,
/// 它的所有前驱已处理完」, 而 `block_order` 是创建序 (含循环时不满足该性质)。
/// DFS 按 `ordered_block_names` 的固定后缀去重, 保证迭代顺序可复现。
fn rpo_order(func: &Function) -> Vec<String> {
    let mut post: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<(String, bool)> = vec![(func.entry.clone(), false)];
    while let Some((b, expanded)) = stack.pop() {
        if expanded {
            post.push(b.clone());
            continue;
        }
        if !seen.insert(b.clone()) {
            continue;
        }
        stack.push((b.clone(), true));
        let Some(bb) = func.blocks.get(&b) else {
            continue;
        };
        // 逆序入栈, 使弹出顺序 = 后继声明顺序 (可复现)。
        for s in succs_of(&bb.term).into_iter().rev() {
            if !seen.contains(&s) {
                stack.push((s, false));
            }
        }
    }
    post.reverse();
    post
}

/// 操作数的「身份串」: SSA 下变量名已按定义唯一, 因此它本身就是值号。
fn operand_identity(o: &Operand) -> String {
    match o {
        Operand::Var(v) => v.name(),
        Operand::Int(i) => format!("#i{i}"),
        Operand::Float(f, ty) => format!("#f{:?}{}", f, type_tag_of(ty)),
        Operand::Str(s) => format!("#s{}", s.len()),
        Operand::SymAddr(n, _) => format!("#sym{n}"),
        Operand::FnAddr(n, _) => format!("#fn{n}"),
        Operand::Void => "#void".to_string(),
        Operand::Call(n) => format!("#call{n}"),
        Operand::Phi(_) => "#phi".to_string(),
    }
}

/// 类型标签: 参与值号时必须能区分不同类型 (`Cast` 的目标准类型与 `Bin` 的宽度)。
fn type_tag_of(ty: &Type) -> String {
    format!("{:?}", ty)
}

/// 把一处引用从一个 SSA 名改写到另一个 SSA 名 (递归作用于指令、phi 与终结符)。
fn rename_uses(func: &mut Function, map: &HashMap<String, SsaVar>) {
    if map.is_empty() {
        return;
    }
    for bb in func.blocks.values_mut() {
        for phi in &mut bb.phis {
            for (v, _) in &mut phi.incoming {
                if let Some(n) = map.get(&v.name()) {
                    *v = n.clone();
                }
            }
        }
        for inst in &mut bb.insts {
            rename_in_kind(&mut inst.kind, map);
        }
        rename_in_term(&mut bb.term, map);
    }
}

fn rename_var(v: &mut SsaVar, map: &HashMap<String, SsaVar>) {
    if let Some(n) = map.get(&v.name()) {
        *v = n.clone();
    }
}

fn rename_op(o: &mut Operand, map: &HashMap<String, SsaVar>) {
    if let Operand::Var(v) = o {
        rename_var(v, map);
    }
}

/// 穷举: 新增 InstKind 形态时本函数编译失败, 强制同步 —— 漏一个操作数位置就等于
/// 重写后仍有旧名字引用被删除的定义, 直接产生悬空 SSA 名。
fn rename_in_kind(k: &mut InstKind, map: &HashMap<String, SsaVar>) {
    match k {
        InstKind::Bin { l, r, .. } => {
            rename_op(l, map);
            rename_op(r, map);
        },
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } => rename_op(e, map),
        InstKind::AddrOf { .. } => {},
        InstKind::Load { e, .. } => rename_op(e, map),
        InstKind::Store { e, src, .. } => {
            rename_op(e, map);
            rename_op(src, map);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            rename_op(arr, map);
            rename_op(idx, map);
        },
        InstKind::IndexAddr { arr, idx, .. } => {
            rename_op(arr, map);
            rename_op(idx, map);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            rename_op(arr, map);
            rename_op(idx, map);
            rename_op(src, map);
        },
        InstKind::Call { callee, args, .. } => {
            rename_op(callee, map);
            for a in args {
                rename_op(a, map);
            }
        },
        InstKind::Invoke { callee, args, .. } => {
            rename_op(callee, map);
            for a in args {
                rename_op(a, map);
            }
        },
        InstKind::MemberLoad { obj, .. } | InstKind::MemberAddr { obj, .. } => rename_op(obj, map),
        InstKind::MemberStore { obj, src, .. } => {
            rename_op(obj, map);
            rename_op(src, map);
        },
        InstKind::Select {
            cond, then_, else_, ..
        } => {
            rename_op(cond, map);
            rename_op(then_, map);
            rename_op(else_, map);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for i in inputs {
                rename_op(i, map);
            }
        },
        InstKind::PreservedDecl { .. } => {},
        InstKind::EhLandingPad { selector } => rename_var(selector, map),
    }
}

fn rename_in_term(t: &mut Terminator, map: &HashMap<String, SsaVar>) {
    match t {
        Terminator::Ret { val: Some(v) } => rename_op(v, map),
        Terminator::Ret { val: None } => {},
        Terminator::Branch { cond, .. } => rename_op(cond, map),
        Terminator::SwitchTable { idx, .. } => rename_op(idx, map),
        Terminator::Jump(..) | Terminator::Unreachable => {},
    }
}

/// 一个内存访问的「位置」标识 —— DSE 只能在不引入别名的前提下删写。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum StoreLoc {
    /// 具名对象 (局部变量或文件作用域对象) 的本体: `&x` 的 x。
    Named(String),
    /// 一个指针值所指的对象, 名字不可知 (包活 `*p` / `p->f` / `p[i]`)。
    Via(String),
    /// 完全无法用名字表达 (如 `*(a + i)` 的计算结果地址) ⇒ 与一切位置潜在别名。
    Opaque,
}

/// 从地址/基址操作数推出被访问位置。`defs` 是 SSA 名 -> 定义它的指令。
fn access_loc(addr: &Operand, defs: &HashMap<String, InstKind>) -> StoreLoc {
    match addr {
        Operand::SymAddr(n, _) => {
            // `SymAddr(name)` 作为地址操作数 = 「以该符号的值为地址」(指针型全局)。
            StoreLoc::Via(format!("sym:{n}"))
        },
        Operand::Var(v) => {
            if let Some(kind) = defs.get(&v.name()) {
                if let InstKind::AddrOf { base } = kind {
                    return StoreLoc::Named(base.clone());
                }
                if let InstKind::IndexLoad { arr, idx, .. } = kind {
                    // 指针算术 `(p + i)`: 基址可知名则记到基址上, 否则 Via/Opaque。
                    let _ = idx;
                    return access_loc(arr, defs);
                }
                if let InstKind::MemberLoad { obj, .. } = kind {
                    return access_loc(obj, defs);
                }
                return StoreLoc::Via(v.name());
            }
            StoreLoc::Via(v.name())
        },
        _ => StoreLoc::Opaque,
    }
}

// =====================================================================
// P17. GVN (全局值编号)
// =====================================================================

/// 一个纯运算的「表达式形状」: 运算 + 各操作数的值身份。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum ValKey {
    Bin(String, String, String),
    Un(String, String),
    Cast(String, String),
    Select(String, String, String),
    /// 内存读 (仅在同一块内且无穿插写时可复用)
    Load(String),
    Member(String, String, String),
    Index(String, String, String),
}

/// 全局值编号 (GVN)：在 SSA IR 上把「形状完全相同的计算」归一到同一个值。
///
/// 与 AST 层的 `ast_gvn` 不同, 这里能安全地跳块复用, 因为 SSA 名**不可变**:
/// `y = a + b` 与 `x = a + b` 里的 a/b 是同一个定义, 在任何使用点它们就是同一个值,
/// 不存在 AST 名字模型里那种「a 后来被改了」的 kill 问题。
/// 例外是内存读: 本 IR 没有 memory SSA, store 不进任何名字链 ⇒ 读的同形不同点
/// **不能**跳块复用, 只在当前块内、且中途没有写/调用/汇编时复用。
///
/// 做了什么:
///   1. 跳块传播纯表达式的可用性: `in[BB] = ⋂ out[pred]` (交集要求同 key 同值名),
///      按 RPO 迭代到不动点;
///   2. 命中的冗余计算不重新发射: dest 重命名到已有值, 本指令删除;
///   3. 退化 phi (所有 incoming 同一个值) 直接换成该值; 同块内形状相同的 phi 归一;
///   4. 整轮应用改名, 反复直到不再变化 (有上限)。
pub fn gvn(func: &mut Function) -> bool {
    let mut total_changed = false;
    // 上限: 改名是单调收敛 (每轮至少多一个归一), 给足够轮数并依靠「不再变化」提前退出。
    for _round in 0..16 {
        let defs = collect_defs(func);
        let var_table = collect_var_table(func);
        let order = rpo_order(func);
        let preds = preds_from_terminators(func);
        let mut out_table: HashMap<String, HashMap<ValKey, String>> = HashMap::new();
        let mut renames: HashMap<String, SsaVar> = HashMap::new();
        let mut dead: HashMap<String, HashSet<String>> = HashMap::new(); // bb -> 冗余指令 dest 名
        let mut phi_dead: HashMap<String, HashSet<String>> = HashMap::new();
        // 本轮是否有过「保留指令、只改右式」的改写 (这类改写不产生 renames/dead)。
        let mut rewrote_in_place = false;
        // 用户可见变量名集 (与 DSE / dead_inst_elim 用同一判据的真源)。
        let named_before = local_object_names(func);

        for bb_name in &order {
            // 1. in = 各前驱 out 的交集 (交集要求「同形状且同一值名」); 无前驱 ⇒ 空表。
            let mut cur: HashMap<ValKey, String> = HashMap::new();
            let my_preds: Vec<String> = preds.get(bb_name).cloned().unwrap_or_default();
            if let Some((first_pred, rest)) = my_preds.split_first() {
                if let Some(t) = out_table.get(first_pred) {
                    cur = t.clone();
                }
                for p in rest {
                    let Some(t) = out_table.get(p) else {
                        cur.clear();
                        break;
                    };
                    cur.retain(|k, n| t.get(k).map(|m| m == n).unwrap_or(false));
                }
            }

            // 2. 本块内的表与逐指令编号。`local` 只在本块内复用, 并且遇任何
            //    写/调用/汇编即作废 (内存读与浮点运算共用这条保守规则)。
            let mut local: HashMap<ValKey, String> = HashMap::new();
            let Some(bbm) = func.blocks.get_mut(bb_name) else {
                continue;
            };
            for idx in 0..bbm.insts.len() {
                let Some(dest) = bbm.insts[idx].dest.clone() else {
                    // 无 dest 的指令 (store 等) 是写屏障: 本块内可用表归零。
                    if inst_is_side_effecting(&bbm.insts[idx].kind) {
                        local.clear();
                    }
                    continue;
                };
                if dest.name().starts_with("__dcc_") {
                    continue;
                }
                let Some(key) = val_key(&bbm.insts[idx].kind, &defs) else {
                    // 不纯/不可编号: 它本身是一次写或调用 ⇒ 本块内可用表作废
                    if inst_is_side_effecting(&bbm.insts[idx].kind) {
                        local.clear();
                    }
                    continue;
                };
                // 三类可用性不同, 必须区分:
                //   * 内存读 (Load/Member/Index): 本 IR 没有 memory SSA, store 不进任何
                //     名字链 ⇒ 只能在本块内、且中途无写时复用;
                //   * 浮点相关运算: 舍入模式可被 `fesetround` 在运行期改掉, 跳块或跨调用
                //     复用会把旧模式下的结果发给新模式 (实测 `fenv_round.c`: DOWNWARD 与
                //     UPWARD 下的两次 `1.0/3.0` 被归一, `down != up` 判定失效); FENV_ACCESS
                //     ON 时异常标志也可观测, 连本块内复用都禁止;
                //   * 其余纯运算 (整数): SSA 名不可变, 可安全跳块传递。
                let fp_sensitive = is_fp_flavored(&bbm.insts[idx].kind, &dest.ty);
                if fp_sensitive && func.fenv_access {
                    continue;
                }
                let local_only = fp_sensitive
                    || matches!(
                        &key,
                        ValKey::Load(_) | ValKey::Member(..) | ValKey::Index(..)
                    );
                let table = if local_only { &mut local } else { &mut cur };
                let hit = table.get(&key).cloned();
                match hit {
                    Some(prev) => {
                        // 命中的是已存在的同形计算。两种处理必须区分:
                        //   * dest 是临时值 (`_t*` 等) → 直接把它归一到已有值, 本指令删除;
                        //   * dest 基名是**用户可见变量** → 绝不能删! 重建阶段这条指令就是
                        //     `y = …` 这条赋值语句的唯一来源, 删它等于删赋值。循环里尤其致命:
                        //     累加 `s = s + i` 被归一到上一轮的值后, `s` 再也不变
                        //     (实测解锁后 `cont(4)` 由 5 变 0)。
                        //     这里保留指令, 只把右式换成一次取值。
                        if named_before.contains(&dest.base) {
                            // 已经就是「取 prev 值」的拷贝 ⇒ 无可再改, 不要谎报变化
                            // (否则每轮都会因 `rewrote_in_place` 而跑满 16 轮)。
                            let already_copy = matches!(&bbm.insts[idx].kind, InstKind::Bin {
                                op: BinOp::Add,
                                l: Operand::Var(v),
                                r: Operand::Int(0),
                            } if v.name() == prev);
                            if !already_copy {
                                if let Some(target) = var_table.get(&prev) {
                                    bbm.insts[idx].kind = InstKind::Bin {
                                        op: BinOp::Add,
                                        l: Operand::Var(target.clone()),
                                        r: Operand::Int(0),
                                    };
                                    rewrote_in_place = true;
                                }
                            }
                        } else {
                            // **只有改写真的记下了才能删除该指令**。`register_rename` 会拒绝
                            // 同一个源名的第二次改写, 若此时仍把它放进 `dead`, 就会留下
                            // 指向已删定义的引用: 实测 `if (_tbin_2)` 读一个从未赋值的临时名,
                            // `cont(4)` 由 5 变 0。
                            if register_rename(&mut renames, &var_table, &dest, &prev) {
                                dead.entry(bb_name.clone())
                                    .or_default()
                                    .insert(dest.name());
                            }
                        }
                    },
                    None => {
                        table.insert(key, dest.name());
                    },
                }
            }
            out_table.insert(bb_name.clone(), cur);

            // 3. phi 退化与同块合并。
            let mut seen_phi: HashMap<Vec<(String, String)>, String> = HashMap::new();
            for phi in &bbm.phis {
                let names: Vec<String> = phi.incoming.iter().map(|(v, _)| v.name()).collect();
                if let Some(first) = names.iter().find(|n| **n != phi.dest.name()) {
                    if names.iter().all(|n| n == first) {
                        // 所有入边同一个值 ⇒ phi 退化 (改写失败则不得删除, 见 `register_rename`)。
                        if register_rename(&mut renames, &var_table, &phi.dest, first) {
                            phi_dead
                                .entry(bb_name.clone())
                                .or_default()
                                .insert(phi.dest.name());
                        }
                        continue;
                    }
                }
                let shape: Vec<(String, String)> = phi
                    .incoming
                    .iter()
                    .map(|(v, p)| (v.name(), p.clone()))
                    .collect();
                if let Some(prev) = seen_phi.get(&shape) {
                    if register_rename(&mut renames, &var_table, &phi.dest, prev) {
                        phi_dead
                            .entry(bb_name.clone())
                            .or_default()
                            .insert(phi.dest.name());
                    }
                } else {
                    seen_phi.insert(shape, phi.dest.name());
                }
            }
        }

        if renames.is_empty() && !rewrote_in_place {
            break;
        }
        // 4. 应用本轮改名 + 删除冗余指令/phi。
        rename_uses(func, &renames);
        for (bb_name, names) in &dead {
            if let Some(bb) = func.blocks.get_mut(bb_name) {
                bb.insts
                    .retain(|i| !i.dest.as_ref().is_some_and(|d| names.contains(&d.name())));
            }
        }
        for (bb_name, names) in &phi_dead {
            if let Some(bb) = func.blocks.get_mut(bb_name) {
                bb.phis.retain(|p| !names.contains(&p.dest.name()));
            }
        }
        total_changed = true;
    }
    total_changed
}

/// 登记一次「from → to」改名。返回 `false` 表示该源名已有改写(或被自己映射), 调用方
/// **必须**因此放弃删除 from 的定义, 否则就会造出悬空引用。
fn register_rename(
    renames: &mut HashMap<String, SsaVar>,
    var_table: &HashMap<String, SsaVar>,
    from: &SsaVar,
    to: &str,
) -> bool {
    if from.name() == to || renames.contains_key(&from.name()) {
        return false;
    }
    match var_table.get(to) {
        Some(target) => {
            renames.insert(from.name(), target.clone());
            true
        },
        None => false,
    }
}

/// SSA 名 -> 定义它的指令形态 (用于把 `AddrOf` 等地址来源识别为具名位置)。
fn collect_defs(func: &Function) -> HashMap<String, InstKind> {
    let mut m: HashMap<String, InstKind> = HashMap::new();
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                m.insert(d.name(), inst.kind.clone());
            }
        }
    }
    m
}

/// 全部 SSA 名 -> SsaVar (改名时需要目标变量的完整类型/span)。
fn collect_var_table(func: &Function) -> HashMap<String, SsaVar> {
    let mut m: HashMap<String, SsaVar> = HashMap::new();
    for p in &func.params {
        m.insert(p.name(), p.clone());
    }
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                m.insert(d.name(), d.clone());
            }
        }
        for phi in &bb.phis {
            m.insert(phi.dest.name(), phi.dest.clone());
        }
    }
    m
}

/// 纯运算的表达式形状; `None` = 不参与值编号 (有副作用、读内存但带 volatile、
/// 戒指数等无法保证重复执行语义一致的形态)。
fn val_key(k: &InstKind, _defs: &HashMap<String, InstKind>) -> Option<ValKey> {
    match k {
        InstKind::Bin { op, l, r } => {
            // 除/模可能陷阱: 多执行一次会新增可观察行为, 仅当已命中同样形状时
            // 才能删 (而删的是重复执行, 不会新增陷阱) —— 故允许入表与复用。
            Some(ValKey::Bin(
                format!("{op:?}"),
                operand_identity(l),
                operand_identity(r),
            ))
        },
        InstKind::Un { op, e } => Some(ValKey::Un(format!("{op:?}"), operand_identity(e))),
        InstKind::Cast { ty, e } => Some(ValKey::Cast(type_tag_of(ty), operand_identity(e))),
        InstKind::Select {
            cond, then_, else_, ..
        } => Some(ValKey::Select(
            operand_identity(cond),
            operand_identity(then_),
            operand_identity(else_),
        )),
        InstKind::Load {
            e, ty, is_volatile,
        } if !is_volatile => Some(ValKey::Load(format!(
            "{}:{}", operand_identity(e), type_tag_of(ty)
        ))),
        InstKind::MemberLoad {
            obj, field, ty, ..
        } => Some(ValKey::Member(
            operand_identity(obj),
            field.clone(),
            type_tag_of(ty),
        )),
        InstKind::IndexLoad { arr, idx, ty, .. } => Some(ValKey::Index(
            operand_identity(arr),
            operand_identity(idx),
            type_tag_of(ty),
        )),
        _ => None,
    }
}

// =====================================================================
// P18. DSE (死存储消除)
// =====================================================================

/// 从某条写往后的「访问序列」元素。
#[derive(Clone, PartialEq, Debug)]
enum FutureAccess {
    /// 一次写 (位置, 是否 volatile)
    Write(StoreLoc, bool),
    /// 一次内存读 (位置, 是否 volatile)
    Read(StoreLoc, bool),
    /// 读写集合未知的访问 (调用 / asm 带内存副作用 / EH 交接)
    Opaque,
}

/// 死存储消除: 删掉「后续再没人读」的写入, 减少内存访问与指令数。
///
/// 判据 (局部 + 沿单入链的全局):
///   * 从本写往后走: 先遇到「同位置的写」⇒ 本写被覆盖 ⇒ 死; 先遇到任何可能读该位置
///     的访问 ⇒ 活。走到一条**无后继**的终点 (Ret/Unreachable) 且全程无人读 ⇒ 死。
///   * 链只沿「后继唯一前驱」扩展, 分支处直接截断当不完整 ⇒ 宁少删不错删。
///   * `is_volatile` 写永不删 (可观测副作用, 与有没有人读无关); volatile 读一律当读者。
///   * 位置不可名 (`*p` / `p->f` / 计算出的地址) 的写永不删 —— 本层没有 memory SSA,
///     无法证明它不与其仙读写别名。
///   * 只删「本函数拥有的局部对象」: 文件作用域对象可被其他 TU / 被调者读取,
///     本层看不到那些访问。被取址并**以非直接寻址方式消费**的名字 (递送给出参、
///     存入内存、返回、参与指针算术、进 asm) 也算逃逸, 写不可删。
pub fn dead_store_elim(func: &mut Function) -> bool {
    // 每轮只删一条: 删除同时改变下标与「单入链」形态, 逐条重算最可靠 (死写量级很小)。
    let mut changed_any = false;
    loop {
        let defs = collect_defs(func);
        let escapes = escaping_bases(func);
        let locals = local_object_names(func);
        let preds = preds_from_terminators(func);
        let Some((bb_name, idx)) = find_dead_store(func, &defs, &escapes, &locals, &preds) else {
            break;
        };
        if let Some(bb) = func.blocks.get_mut(&bb_name) {
            if idx < bb.insts.len() {
                bb.insts.remove(idx);
                changed_any = true;
            }
        }
    }
    changed_any
}

/// 该运算的结果是否**受浮点环境影响** (自己或操作数涉浮点)。
fn is_fp_flavored(k: &InstKind, dest_ty: &Type) -> bool {
    if is_float_ty(dest_ty) {
        return true;
    }
    let op_fp = |o: &Operand| match o {
        Operand::Float(..) => true,
        Operand::Var(v) => is_float_ty(&v.ty),
        _ => false,
    };
    match k {
        InstKind::Bin { l, r, .. } => op_fp(l) || op_fp(r),
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } | InstKind::Load { e, .. } => op_fp(e),
        InstKind::Select {
            cond, then_, else_, ..
        } => op_fp(cond) || op_fp(then_) || op_fp(else_),
        InstKind::IndexLoad { idx, .. } => op_fp(idx),
        _ => false,
    }
}

fn is_float_ty(ty: &Type) -> bool {
    match ty {
        Type::Float | Type::Double | Type::LongDouble => true,
        Type::Complex(inner) | Type::Imaginary(inner) => is_float_ty(&inner.base),
        Type::Vector(inner, _) => is_float_ty(&inner.base),
        _ => false,
    }
}

/// 本函数拥有的对象名 (局部 + 参数)。文件作用域对象的写不在本判据的作删范内。
fn local_object_names(func: &Function) -> HashSet<String> {
    let mut s: HashSet<String> = func.original_locals.iter().cloned().collect();
    for p in &func.params {
        s.insert(p.base.clone());
    }
    s
}

fn store_of(k: &InstKind, defs: &HashMap<String, InstKind>) -> Option<(StoreLoc, bool)> {
    match k {
        InstKind::Store {
            e, is_volatile, ..
        } => Some((access_loc(e, defs), *is_volatile)),
        InstKind::IndexStore {
            arr, is_volatile, ..
        } => Some((access_loc(arr, defs), *is_volatile)),
        InstKind::MemberStore {
            obj, is_volatile, ..
        } => Some((access_loc(obj, defs), *is_volatile)),
        _ => None,
    }
}

/// 把一条指令对内存的影响归一成 `FutureAccess`; 纯值运算返回 `None` (不影哩判定)。
fn access_of(k: &InstKind, defs: &HashMap<String, InstKind>) -> Option<FutureAccess> {
    match k {
        InstKind::Load {
            e, is_volatile, ..
        } => Some(FutureAccess::Read(access_loc(e, defs), *is_volatile)),
        InstKind::IndexLoad {
            arr, is_volatile, ..
        } => Some(FutureAccess::Read(access_loc(arr, defs), *is_volatile)),
        InstKind::MemberLoad {
            obj, is_volatile, ..
        } => Some(FutureAccess::Read(access_loc(obj, defs), *is_volatile)),
        InstKind::Call { .. } | InstKind::Invoke { .. } | InstKind::EhLandingPad { .. } => {
            Some(FutureAccess::Opaque)
        },
        // 只有 volatile 或带内存约束/clobber 的 asm 才影响内存。
        InstKind::InlineAsm {
            volatile,
            clobbers,
            input_constraints,
            output_constraints,
            ..
        } if *volatile
            || asm_has_memory_clobber(clobbers)
            || asm_constraints_touch_memory(input_constraints)
            || asm_constraints_touch_memory(output_constraints) =>
        {
            Some(FutureAccess::Opaque)
        },
        _ => store_of(k, defs).map(|(l, v)| FutureAccess::Write(l, v)),
    }
}

fn find_dead_store(
    func: &Function,
    defs: &HashMap<String, InstKind>,
    escapes: &HashSet<String>,
    locals: &HashSet<String>,
    preds: &HashMap<String, Vec<String>>,
) -> Option<(String, usize)> {
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        for idx in 0..bb.insts.len() {
            let Some((loc, volatile)) = store_of(&bb.insts[idx].kind, defs) else {
                continue;
            };
            let StoreLoc::Named(name) = loc else {
                continue;
            };
            if volatile || !locals.contains(&name) || escapes.contains(&name) {
                continue;
            }
            let mut future: Vec<FutureAccess> = Vec::new();
            let reached_end =
                collect_future(func, &bb_name, idx + 1, defs, preds, &mut future);
            if is_dead_store(&name, &future, escapes, reached_end) {
                return Some((bb_name.clone(), idx));
            }
        }
    }
    None
}

/// 从 (`bb`, `from_idx`) 往后收集访问序列。返回 `true` = 走到了**无后继**的终点
/// (序列完整); `false` = 在不完整边界截断 (分支 / 后继多前驱 / 环)。
fn collect_future(
    func: &Function,
    bb: &str,
    from_idx: usize,
    defs: &HashMap<String, InstKind>,
    preds: &HashMap<String, Vec<String>>,
    out: &mut Vec<FutureAccess>,
) -> bool {
    let mut cur = bb.to_string();
    let mut start = from_idx;
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(bb.to_string());
    loop {
        let Some(b) = func.blocks.get(&cur) else {
            return true;
        };
        for inst in b.insts.iter().skip(start) {
            if let Some(a) = access_of(&inst.kind, defs) {
                out.push(a);
            }
        }
        start = 0;
        let succs = succs_of(&b.term);
        match succs.as_slice() {
            [] => return true,
            [s] => {
                // 只有「后继的唯一前驱就是本块」才能肯定本块后面紧接后继。
                let sole_pred = preds
                    .get(s)
                    .map(|p| p.len() == 1 && p[0] == cur)
                    .unwrap_or(false);
                if sole_pred && visited.insert(s.clone()) {
                    cur = s.clone();
                    continue;
                }
                return false;
            },
            _ => return false,
        }
    }
}

/// 序列判定: 被后续同位置写覆盖, 或整条未来路径上无人读 ⇒ 死。
fn is_dead_store(
    name: &str,
    future: &[FutureAccess],
    escapes: &HashSet<String>,
    reached_end: bool,
) -> bool {
    for a in future {
        match a {
            FutureAccess::Write(StoreLoc::Named(n), _) => {
                if n == name {
                    return true;
                }
                if escapes.contains(n) {
                    return false;
                }
            },
            // 不具名/不透明的写可能覆盖本位置。
            FutureAccess::Write(_, _) => return false,
            FutureAccess::Read(StoreLoc::Named(n), vol) => {
                if *vol || n == name || escapes.contains(n) {
                    return false;
                }
            },
            FutureAccess::Read(_, _) | FutureAccess::Opaque => return false,
        }
    }
    // 序列走完了 (整条未来路径无后继) 仍无人读 ⇒ 本写确实死。
    reached_end
}

/// 哪些 `AddrOf` 的基址真正**逃逸**了本帧。
///
/// 不能简单的「出现过 `AddrOf` 就算逃逸」: 数组/结构体的元素访问本身就在发
/// `AddrOf{base}` + `IndexStore{arr: Var(addr)}`, 那是**帧内寻址**, 不是逃逸。真
/// 正的逃逸是取出的地址被「非直接访存地址」的位置消费: 当参数递给出参、存进内存、
/// 作为返回值、参与指针算术、进 asm、或经 phi 流通到任意路径。
fn escaping_bases(func: &Function) -> HashSet<String> {
    let mut addr_of_dest: HashMap<String, String> = HashMap::new();
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            if let (Some(d), InstKind::AddrOf { base }) = (&inst.dest, &inst.kind) {
                addr_of_dest.insert(d.name(), base.clone());
            }
        }
    }
    let mut escape = HashSet::new();
    // 一个名字被“可逃位置”引用 ⇒ 其基址逃逸。
    let mut mark = |name: &str, escape: &mut HashSet<String>| {
        if let Some(base) = addr_of_dest.get(name) {
            escape.insert(base.clone());
        }
    };
    for bb in func.blocks.values() {
        // phi 入边的地址可被任意路径拿到的 ⇒ 逃逸
        for phi in &bb.phis {
            for (v, _) in &phi.incoming {
                mark(&v.name(), &mut escape);
            }
        }
        for inst in &bb.insts {
            match &inst.kind {
                // 地址位置 (store/load 的落点) 不算逃逸; 其余位置全部算。
                InstKind::Store { src, .. } => mark_op(src, &mut mark, &mut escape),
                InstKind::IndexStore { src, idx, .. } => {
                    mark_op(src, &mut mark, &mut escape);
                    mark_op(idx, &mut mark, &mut escape);
                },
                InstKind::MemberStore { src, .. } => mark_op(src, &mut mark, &mut escape),
                // `&obj.field`：obj 在这里被当**值**（基址指针）读，跟 `MemberLoad` 一样
                // 计为一次使用；地址本身不是逃逸出去的新对象。
                InstKind::MemberAddr { obj, .. } => mark_op(obj, &mut mark, &mut escape),
                InstKind::Call { callee, args, .. } => {
                    mark_op(callee, &mut mark, &mut escape);
                    for a in args {
                        mark_op(a, &mut mark, &mut escape);
                    }
                },
                InstKind::Invoke { callee, args, .. } => {
                    mark_op(callee, &mut mark, &mut escape);
                    for a in args {
                        mark_op(a, &mut mark, &mut escape);
                    }
                },
                InstKind::Bin { l, r, .. } => {
                    mark_op(l, &mut mark, &mut escape);
                    mark_op(r, &mut mark, &mut escape);
                },
                InstKind::Un { e, .. } | InstKind::Cast { e, .. } => mark_op(e, &mut mark, &mut escape),
                // `Load{e}` 的 `e` 是**地址位置** (读那个对象), 不是逃逸: 把
                // `_addr = &x; v = *_addr;` 算成逃逸会让 `x` 的写永远删不掉。
                InstKind::Load { .. } => {},
                InstKind::IndexLoad { idx, .. } => mark_op(idx, &mut mark, &mut escape),
                // `IndexAddr` 与 `IndexLoad` 同理：`arr` 是地址位置，只有下标算使用。
                InstKind::IndexAddr { idx, .. } => mark_op(idx, &mut mark, &mut escape),
                InstKind::Select {
                    cond, then_, else_, ..
                } => {
                    mark_op(cond, &mut mark, &mut escape);
                    mark_op(then_, &mut mark, &mut escape);
                    mark_op(else_, &mut mark, &mut escape);
                },
                InstKind::InlineAsm { inputs, outputs, .. } => {
                    for i in inputs {
                        mark_op(i, &mut mark, &mut escape);
                    }
                    // asm 输出是 AST 左值: 里面提到哪个名字就是什么地址被外部使用。
                    for o in outputs {
                        let mut names = Vec::new();
                        collect_expr_uses(o, &mut names);
                        for n in names {
                            mark(&n, &mut escape);
                        }
                    }
                },
                InstKind::AddrOf { .. } | InstKind::PreservedDecl { .. } => {},
                InstKind::MemberLoad { .. } | InstKind::EhLandingPad { .. } => {},
            }
            // 指令结果本身就是地址时 (如 `p = &x + 1` 的存归), dest 不参与判定:
            // 上面的各位置已覆盖所有会“拿着地址跑”的操作数。
            let _ = &inst.dest;
        }
        match &bb.term {
            Terminator::Ret { val: Some(v) } => mark_op(v, &mut mark, &mut escape),
            Terminator::Branch { cond, .. } => mark_op(cond, &mut mark, &mut escape),
            Terminator::SwitchTable { idx, .. } => mark_op(idx, &mut mark, &mut escape),
            _ => {},
        }
    }
    escape
}

fn mark_op<F>(o: &Operand, mark: &mut F, escape: &mut HashSet<String>)
where
    F: FnMut(&str, &mut HashSet<String>),
{
    if let Operand::Var(v) = o {
        mark(&v.name(), escape);
    }
}

// =====================================================================
// P19. Jump Threading (跳转穿线)
// =====================================================================

/// 跳转穿线: 消除常量分支与空跳转链, 减少冗余跳转与不可达块。
///
/// 本实现做两件确定性的事 (均需重算 phi 与 CFG 缓存):
///   1. **常量分支折叠**: `Branch` 的条件已是常量 (`Operand::Int`) ⇒ 退化 `Jump(target)`,
///      并从未走到的后继里剔掉本块那条 phi 入边 (入边不足两个时 phi 退化);
///   2. **空跳转链合并**: `Jump(t)` 且 t 无 phi 无指令 ⇒ 直接改跳到 t 的目标, t 随后
///      因不可达被除。
/// 两者叠代到不变化 (有上限): 一轮折叠会暴露下一轮的常量条件。
///
/// 更激进的穿线 (沿支配路径把上游常量条件复制下来、重定向入边) 需要改 CFG 拓扑与
/// phi 拼边, 不在本层做 —— 本层只做能证明等价形态的变换。
pub fn jump_threading(func: &mut Function) -> bool {
    let mut changed_any = false;
    for _round in 0..16 {
        let mut changed_this_round = false;
        // 1. 常量分支折叠
        for bb_name in ordered_block_names(func) {
            let Some(bb) = func.blocks.get(&bb_name) else {
                continue;
            };
            let taken = match &bb.term {
                Terminator::Branch {
                    cond: Operand::Int(v),
                    t,
                    f,
                } => Some(if *v != 0 { t.clone() } else { f.clone() }),
                _ => None,
            };
            let Some(target) = taken else { continue };
            let dropped = match &bb.term {
                Terminator::Branch { t, f, .. } => {
                    if *t == target {
                        f.clone()
                    } else {
                        t.clone()
                    }
                },
                _ => continue,
            };
            if let Some(b) = func.blocks.get_mut(&bb_name) {
                b.term = Terminator::Jump(target);
                changed_this_round = true;
            }
            prune_phi_incoming(func, &dropped, &bb_name);
        }
        // 2. 空跳转链合并: `Jump(t)` 且 t 是「无指令、无 phi」的纯跳板 ⇒ 直接指到 t 的目标。
        let mut redirs: Vec<(String, String)> = Vec::new();
        for bb_name in ordered_block_names(func) {
            let Some(bb) = func.blocks.get(&bb_name) else {
                continue;
            };
            let Terminator::Jump(t) = &bb.term else {
                continue;
            };
            if t == &bb_name {
                continue; // 自环不是链
            }
            let Some(tb) = func.blocks.get(t) else {
                continue;
            };
            if !tb.insts.is_empty() || !tb.phis.is_empty() {
                continue;
            }
            let Terminator::Jump(u) = &tb.term else {
                continue;
            };
            if u == t {
                continue;
            }
            redirs.push((bb_name.clone(), u.clone()));
        }
        for (b, u) in redirs {
            if let Some(bb) = func.blocks.get_mut(&b) {
                if matches!(&bb.term, Terminator::Jump(t) if *t != u) {
                    bb.term = Terminator::Jump(u);
                    changed_this_round = true;
                }
            }
        }
        // 3. 不可达块剔除 (沿终结符派生的后继, 不读可能陈旧的 `bb.succs`)
        if drop_unreachable_blocks(func) {
            changed_this_round = true;
        }
        // 4. 块被剔除后, 其他块的 phi 可能留着指向已不存在前驱的行; 行数跌到 ≤ 1 时
        //    phi 不再是合并点。不修则重建时读一个永不赋值的局部 (静默错值)。
        if repair_phis_after_edge_removal(func) {
            changed_this_round = true;
        }
        refresh_cfg_caches(func);
        if !changed_this_round {
            break;
        }
        changed_any = true;
    }
    changed_any
}

/// 删边后维护目标块的 phi: 剔掉来自已不存在入边的来路; 只剩一个入边时退化。
fn prune_phi_incoming(func: &mut Function, bb_name: &str, pred: &str) {
    let Some(bb) = func.blocks.get_mut(bb_name) else {
        return;
    };
    for phi in &mut bb.phis {
        phi.incoming.retain(|(_, p)| p != pred);
    }
    // 退化 phi: 只剩一个入边时它不再是合并点, 直接用那个值。
    let mut renames: HashMap<String, SsaVar> = HashMap::new();
    let mut drop: Vec<String> = Vec::new();
    for phi in &bb.phis {
        if phi.incoming.len() == 1 {
            renames.insert(phi.dest.name(), phi.incoming[0].0.clone());
            drop.push(phi.dest.name());
        }
    }
    if !drop.is_empty() {
        bb.phis.retain(|p| !drop.contains(&p.dest.name()));
        if !renames.is_empty() {
            // 只作用在本块内: 被退化的 phi 其入边值必然支配本块, 而本块以外不会
            // 再引用这个 phi 的产物 (它在其它块里的引用同样应该改写)。
            // 因此改名必须走全局 `rename_uses`, 不能只改本块。
            drop.clear();
            let snapshot = renames.clone();
            rename_uses(func, &snapshot);
        }
    }
}

/// 不可达块剔除: 从 entry 沿终结符派生的后继可达。
fn drop_unreachable_blocks(func: &mut Function) -> bool {
    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack = vec![func.entry.clone()];
    while let Some(b) = stack.pop() {
        if !reachable.insert(b.clone()) {
            continue;
        }
        let Some(bb) = func.blocks.get(&b) else {
            continue;
        };
        for s in succs_of(&bb.term) {
            if !reachable.contains(&s) {
                stack.push(s);
            }
        }
    }
    let before = func.blocks.len();
    func.blocks.retain(|k, _| reachable.contains(k));
    func.block_order.retain(|k| reachable.contains(k));
    func.blocks.len() != before
}

/// 剔除不可达块之后修补 phi。
///
/// 为何必须有这一步: phi 的行是按**前驱名**登记的。删掉一个前驱块 (常量分支折叠 +
/// 不可达剔除) 后, 目标块的 phi 会留下指向已不存在前驱的行 —— 那行永远不会被赋值,
/// 而 `rebuild_block` 只能把 phi 当成普通局部变量处理, 读到的就是从未写入的栈槽。
/// 行跌到 ≤ 1 时该块已不是合并点, 直接退化成重命名。
fn repair_phis_after_edge_removal(func: &mut Function) -> bool {
    let mut live: HashSet<String> = func.blocks.keys().cloned().collect();
    live.insert(func.entry.clone());
    let mut changed = false;
    // 先剔除死行 (可能一次就把 phi 降到单入边)。
    for bb in func.blocks.values_mut() {
        for phi in &mut bb.phis {
            let before = phi.incoming.len();
            phi.incoming.retain(|(_, p)| live.contains(p));
            if phi.incoming.len() != before {
                changed = true;
            }
        }
    }
    // 退化: 只剩 0/1 个入边的 phi 不再是 phi。0 入边的 dest 已成悬空名, 同样必须除。
    let var_table = collect_var_table(func);
    let mut renames: HashMap<String, SsaVar> = HashMap::new();
    let mut drop: Vec<(String, String)> = Vec::new();
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        for phi in &bb.phis {
            match phi.incoming.len() {
                0 => {
                    drop.push((bb_name.clone(), phi.dest.name()));
                },
                1 => {
                    register_rename(&mut renames, &var_table, &phi.dest, &phi.incoming[0].0.name());
                    drop.push((bb_name.clone(), phi.dest.name()));
                },
                _ => {},
            }
        }
    }
    if !renames.is_empty() {
        rename_uses(func, &renames);
    }
    let dropped_any = !drop.is_empty();
    for (bb_name, dest) in drop {
        if let Some(bb) = func.blocks.get_mut(&bb_name) {
            bb.phis.retain(|p| p.dest.name() != dest);
        }
    }
    changed || !renames.is_empty() || dropped_any
}

/// 刷新 `bb.preds` / `bb.succs` 缓存。**委派给 `ssa::refresh_cfg`** —— 同一份事实
/// 只允许一个真源, 两处各自 `push`/`retain` 就会重新出现「陈旧的 preds」这一类缺陷。
fn refresh_cfg_caches(func: &mut Function) {
    crate::ssa::refresh_cfg(func);
}

// =====================================================================
// P16. 软件流水 —— 真实现已接入（上面 `run_pipelining`），旧占位已删
// =====================================================================
//
// 旧 P16 是空函数体 + 无条件 `modified = true`: 它不改 IR 却报告「改了」,
// 与旧 P12 同一种骗调度器的形态。当年留下三条前置，现在的状态：
//
//   1. ~~`run_ssa_passes` 对含循环函数 `continue`~~ —— 已随 `func_has_loop`
//      闸门删除而消失，含 while/for/do 的函数无条件进 SSA 管线。
//   2. ~~4 层嵌套 + 变量边界 + `&&` 条件的 -O2 误编译~~ —— 已修（见
//      `docs/ssa-pass-upgrade-plan.md` §6/§8-A）；而流水自己的准入也**不依赖**它：
//      嵌在任何循环里的循环一律不流水（`is_nested_in_other_loop`）。
//   3. 本体已真写出来：依赖图 + RecMII/ResMII + epi-cyclic 模调度 + 三段切分 +
//      peel/kernel/排空落地，在 `crate::ssa::pipelining`。两道可判定自检
//      （`InstancePlan::verify` 证 T 序与实例覆盖，`emitted_reads_match` 证基名
//      塌缩下的发射序）不通过就不接入，拒因由 `DCC_PIPE_DUMP` 可查。
//
// `loop_opt.rs` 里那个 AST 层的同名实现也不可计入: 它的「产物」是往循环前插
// `Expr::AsmExpr{asm_str: "#pragma GCC pipelined"}`, 而该字符串在后端只是一条注释
// (实测语料 -O3 汇编里 0 次出现), 同样不影响产物 —— 它属于伪 pass, 已连同其
// 调用点与专用判据一起删除 (与伪 `loop_tiling`/`loop_interchange` 同一处置标准)。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    use crate::diagnostics::Span;
    use crate::ssa::{Function, Inst, InstKind, Operand, SsaVar};

    fn span() -> Span {
        Span::point("t.c", 1, 1)
    }

    fn make_func(name: &str, is_static: bool, calls: Vec<&str>) -> FuncDecl {
        let body_items: Vec<BlockItem> = calls
            .iter()
            .map(|c| {
                BlockItem::Stmt(Stmt::Expr {
                    e: Expr::Call {
                        callee: Box::new(Expr::Ident {
                            name: Ident {
                                name: c.to_string(),
                                span: span(),
                            },
                            span: span(),
                            ty: Type::Int,
                        }),
                        args: vec![],
                        span: span(),
                        ty: Type::Void,
                    },
                    span: span(),
                })
            })
            .collect();
        FuncDecl {
            ret_ty: Type::Int,
            name: Ident {
                name: name.into(),
                span: span(),
            },
            params: vec![],
            body: Some(Block {
                items: body_items,
                span: span(),
            }),
            span: span(),
            is_static,
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
        }
    }

    #[test]
    fn dce_removes_unreachable_static_with_main() {
        let mut funcs = vec![
            make_func("main", false, vec!["helper"]),
            make_func("helper", true, vec![]),
            make_func("dead_func", true, vec![]),
        ];
        let changed = cross_function_dce(&mut funcs, true);
        assert!(changed);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"helper"));
        assert!(!names.contains(&"dead_func"), "unreachable static should be removed");
    }

    #[test]
    fn dce_removes_unreachable_static_without_main() {
        let mut funcs = vec![
            make_func("foo", true, vec![]),
            make_func("bar", false, vec![]),
        ];
        let changed = cross_function_dce(&mut funcs, true);
        assert!(changed, "unreferenced static can be dropped without main");
        let names: Vec<&str> = funcs.iter().map(|f| f.name.name.as_str()).collect();
        assert!(!names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn dce_preserves_extern_and_used() {
        let mut funcs = vec![
            make_func("main", false, vec![]),
            make_func("ext", false, vec![]),
            make_func("dead_static", true, vec![]),
        ];
        // Mark ext as extern
        funcs[1].is_extern = true;
        let changed = cross_function_dce(&mut funcs, true);
        assert!(changed);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"ext"), "extern must be preserved");
        assert!(!names.contains(&"dead_static"));
    }

    fn make_ssa_func() -> Function {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        // 添加一个 UInt 参数 x
        let x_var = SsaVar {
            base: "x".into(),
            version: 0,
            ty: Type::UInt,
            span: span.clone(),
        };
        f.params.push(x_var.clone());
        // entry BB: _t1 = x * 8 (Mul)
        let t1 = SsaVar {
            base: "_t1".into(),
            version: 0,
            ty: Type::UInt,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: Some(t1.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Mul,
                    l: Operand::Var(x_var.clone()),
                    r: Operand::Int(8),
                },
                span: span.clone(),
            });
            // _t2 = x / 4 (Div, unsigned)
            let t2 = SsaVar {
                base: "_t2".into(),
                version: 0,
                ty: Type::UInt,
                span: span.clone(),
            };
            bb.insts.push(Inst {
                dest: Some(t2),
                kind: InstKind::Bin {
                    op: BinOp::Div,
                    l: Operand::Var(x_var),
                    r: Operand::Int(4),
                },
                span: span.clone(),
            });
        }
        f
    }

    #[test]
    fn sr_mul_to_shl_and_unsigned_div_to_shr() {
        let mut f = make_ssa_func();
        let changed = strength_reduction(&mut f);
        assert!(changed);
        let bb = f.blocks.get("entry").unwrap();
        // Mul 8 -> Shl 3
        match &bb.insts[0].kind {
            InstKind::Bin {
                op: BinOp::Shl,
                r: Operand::Int(3),
                ..
            } => {},
            other => panic!("expected Shl 3, got {other:?}"),
        }
        // Div 4 -> Shr 2 (UInt is unsigned)
        match &bb.insts[1].kind {
            InstKind::Bin {
                op: BinOp::Shr,
                r: Operand::Int(2),
                ..
            } => {},
            other => panic!("expected Shr 2, got {other:?}"),
        }
    }

    #[test]
    fn sr_signed_div_not_converted() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let x_var = SsaVar {
            base: "x".into(),
            version: 0,
            ty: Type::Int, // signed!
            span: span.clone(),
        };
        f.params.push(x_var.clone());
        if let Some(bb) = f.blocks.get_mut("entry") {
            let t1 = SsaVar {
                base: "_t1".into(),
                version: 0,
                ty: Type::Int,
                span: span.clone(),
            };
            bb.insts.push(Inst {
                dest: Some(t1),
                kind: InstKind::Bin {
                    op: BinOp::Div,
                    l: Operand::Var(x_var),
                    r: Operand::Int(4),
                },
                span: span.clone(),
            });
        }
        let changed = strength_reduction(&mut f);
        assert!(!changed, "signed Div must not be converted");
        let bb = f.blocks.get("entry").unwrap();
        match &bb.insts[0].kind {
            InstKind::Bin { op: BinOp::Div, .. } => {},
            other => panic!("Div should remain, got {other:?}"),
        }
    }

    #[test]
    fn ivop_detects_loop_induction_var() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        // 创建简单循环: entry -> for.cond -> for.body -> for.step -> for.cond
        f.add_block("for.cond".into());
        f.add_block("for.body".into());
        f.add_block("for.step".into());
        f.block_order = vec![
            "entry".into(),
            "for.cond".into(),
            "for.body".into(),
            "for.step".into(),
        ];
        // 设置 CFG 边
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.term = Terminator::Jump("for.cond".into());
            bb.succs.push("for.cond".into());
        }
        if let Some(bb) = f.blocks.get_mut("for.cond") {
            bb.preds.push("entry".into());
            bb.preds.push("for.step".into());
            bb.term = Terminator::Branch {
                cond: Operand::Int(1),
                t: "for.body".into(),
                f: "for.end".into(),
            };
            bb.succs.push("for.body".into());
        }
        if let Some(bb) = f.blocks.get_mut("for.body") {
            bb.preds.push("for.cond".into());
        }
        // for.step: i_1 = i_0 + 1 (归纳变量更新)
        let i0 = SsaVar {
            base: "i".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        };
        let i1 = SsaVar {
            base: "i".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("for.step") {
            bb.preds.push("for.body".into());
            bb.insts.push(Inst {
                dest: Some(i1.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(i0.clone()),
                    r: Operand::Int(1),
                },
                span: span.clone(),
            });
            bb.term = Terminator::Jump("for.cond".into());
            bb.succs.push("for.cond".into());
        }
        // for.body: _t1 = i * 8 (应被 IVOP 识别)
        let t1 = SsaVar {
            base: "_t1".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("for.body") {
            bb.insts.push(Inst {
                dest: Some(t1),
                kind: InstKind::Bin {
                    op: BinOp::Mul,
                    l: Operand::Var(i0),
                    r: Operand::Int(8),
                },
                span: span.clone(),
            });
        }
        let ivs = find_induction_vars(&f);
        assert!(ivs.contains("i"), "i should be detected as induction var");
        let changed = ivop(&mut f);
        assert!(changed, "ivop should mark Mul(iv, C)");
    }

    #[test]
    fn tco_replaces_self_call_with_jump() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("factorial".into(), Type::Int, span.clone());
        // entry: _t1 = call factorial(n-1); return _t1
        let t1 = SsaVar {
            base: "_t1".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: Some(t1),
                kind: InstKind::Call {
                    callee: Operand::FnAddr("factorial".into(), Type::Int),
                    args: vec![Operand::Int(5)],
                    ret_ty: Type::Int,
                },
                span: span.clone(),
            });
            bb.term = Terminator::Ret {
                val: Some(Operand::Int(0)),
            };
        }
        let changed = tail_call_optimization(&mut f);
        assert!(changed, "TCO should trigger on self-call");
        let bb = f.blocks.get("entry").unwrap();
        assert!(!bb
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::Call { .. })));
        assert!(matches!(bb.term, Terminator::Jump(ref n) if n == "entry"));
    }

    /// 旧 `fap_marks_hot_and_noreturn` 断言「属性会在 entry BB 造出 hint Store」——
    /// 这条断言把缺陷钉住了 (那个 Store 在后端会回退成未初始化栈槽, 运行期 SEGV)。
    /// 现在锁定的不变式是相反一面: 属性不得在 SSA 层造指令, 因为热/冷归属的真源是
    /// AST 属性 (`FuncDecl::is_hot/is_cold/is_noreturn`, 后端直接读), 而 SSA 回写只送 body。
    #[test]
    fn function_attributes_never_fabricate_instructions() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("hot_fn".into(), Type::Int, span.clone());
        f.is_hot = true;
        f.is_noreturn = true;
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: None,
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Int(1),
                    r: Operand::Int(2),
                },
                span: span.clone(),
            });
            bb.term = Terminator::Ret { val: None };
        }
        let before = f.blocks.get("entry").unwrap().insts.len();
        run(&mut f, 3);
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts.len(), before, "属性不得往 entry BB 插指令");
        assert!(
            !bb.insts.iter().any(|i| {
                matches!(&i.kind, InstKind::Store { e: Operand::SymAddr(n, _), .. } if n.starts_with("__dcc_"))
            }),
            "不得向 __dcc_ 前缀的伪符号发内存写: 该符号不存在, 后端会回退成栈槽并在运行期崩"
        );
    }

    #[test]
    fn scheduling_moves_pure_inst_across_non_memory_asm() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let z = SsaVar {
            base: "z".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        let a = SsaVar {
            base: "a".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: None,
                kind: InstKind::InlineAsm {
                    asm_str: "nop".into(),
                    outputs: vec![],
                    output_names: vec![],
                    output_constraints: vec![],
                    output_vars: vec![],
                    inputs: vec![],
                    input_names: vec![],
                    input_constraints: vec![],
                    clobbers: vec!["cc".into()],
                    volatile: false,
                    ty: Type::Void,
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: Some(z.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Int(1),
                    r: Operand::Int(2),
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: Some(a.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Int(3),
                    r: Operand::Int(4),
                },
                span: span.clone(),
            });
        }
        assert!(instruction_scheduling(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts[0].dest.as_ref().map(|v| v.name()), Some("a.1".into()));
        assert_eq!(bb.insts[1].dest.as_ref().map(|v| v.name()), Some("z.1".into()));
        assert!(matches!(bb.insts[2].kind, InstKind::InlineAsm { .. }));
    }

    #[test]
    fn scheduling_keeps_loads_around_memory_clobber_asm() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let z = SsaVar {
            base: "z".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        let a = SsaVar {
            base: "a".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: Some(z.clone()),
                kind: InstKind::Load {
                    e: Operand::SymAddr("p".into(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    ty: Type::Int,
                    is_volatile: false,
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: None,
                kind: InstKind::InlineAsm {
                    asm_str: "".into(),
                    outputs: vec![],
                    output_names: vec![],
                    output_constraints: vec![],
                    output_vars: vec![],
                    inputs: vec![],
                    input_names: vec![],
                    input_constraints: vec![],
                    clobbers: vec!["memory".into()],
                    volatile: true,
                    ty: Type::Void,
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: Some(a.clone()),
                kind: InstKind::Load {
                    e: Operand::SymAddr("q".into(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    ty: Type::Int,
                    is_volatile: false,
                },
                span: span.clone(),
            });
        }
        assert!(!instruction_scheduling(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts[0].dest.as_ref().map(|v| v.name()), Some("z.1".into()));
        assert!(matches!(bb.insts[1].kind, InstKind::InlineAsm { .. }));
        assert_eq!(bb.insts[2].dest.as_ref().map(|v| v.name()), Some("a.1".into()));
    }

    #[test]
    fn scheduling_allows_pure_inst_across_memory_clobber_asm() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let z = SsaVar {
            base: "z".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        let a = SsaVar {
            base: "a".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: Some(z.clone()),
                kind: InstKind::Load {
                    e: Operand::SymAddr("p".into(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    ty: Type::Int,
                    is_volatile: false,
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: None,
                kind: InstKind::InlineAsm {
                    asm_str: "".into(),
                    outputs: vec![],
                    output_names: vec![],
                    output_constraints: vec![],
                    output_vars: vec![],
                    inputs: vec![],
                    input_names: vec![],
                    input_constraints: vec![],
                    clobbers: vec!["memory".into()],
                    volatile: false,
                    ty: Type::Void,
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: Some(a.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Int(1),
                    r: Operand::Int(2),
                },
                span: span.clone(),
            });
        }
        assert!(instruction_scheduling(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        let pos_load = bb
            .insts
            .iter()
            .position(|i| i.dest.as_ref().is_some_and(|v| v.name() == "z.1"))
            .unwrap();
        let pos_asm = bb
            .insts
            .iter()
            .position(|i| matches!(i.kind, InstKind::InlineAsm { .. }))
            .unwrap();
        let pos_pure = bb
            .insts
            .iter()
            .position(|i| i.dest.as_ref().is_some_and(|v| v.name() == "a.1"))
            .unwrap();
        assert!(pos_load < pos_asm, "memory clobber must keep earlier load before asm");
        assert!(pos_pure < pos_asm, "pure instruction may cross memory-clobber asm");
    }

    #[test]
    fn scheduling_honors_asm_input_and_output_dependencies() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let input = SsaVar {
            base: "x".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        let output = SsaVar {
            base: "y".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        let use_output = SsaVar {
            base: "a".into(),
            version: 1,
            ty: Type::Int,
            span: span.clone(),
        };
        if let Some(bb) = f.blocks.get_mut("entry") {
            bb.insts.push(Inst {
                dest: Some(input.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Int(1),
                    r: Operand::Int(2),
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: None,
                kind: InstKind::InlineAsm {
                    asm_str: "mov %1, %0".into(),
                    outputs: vec![Expr::Ident {
                        name: Ident {
                            name: "y".into(),
                            span: span.clone(),
                        },
                        span: span.clone(),
                        ty: Type::Int,
                    }],
                    output_names: vec![None],
                    output_constraints: vec!["=r".into()],
                    output_vars: vec![output.clone()],
                    inputs: vec![Operand::Var(input.clone())],
                    input_names: vec![None],
                    input_constraints: vec!["r".into()],
                    clobbers: vec![],
                    volatile: true,
                    ty: Type::Void,
                },
                span: span.clone(),
            });
            bb.insts.push(Inst {
                dest: Some(use_output.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(output.clone()),
                    r: Operand::Int(1),
                },
                span: span.clone(),
            });
        }
        assert!(!instruction_scheduling(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts[0].dest.as_ref().map(|v| v.name()), Some("x.1".into()));
        assert!(matches!(bb.insts[1].kind, InstKind::InlineAsm { .. }));
        assert_eq!(bb.insts[2].dest.as_ref().map(|v| v.name()), Some("a.1".into()));
    }


    /// DCE eliminates pure (non-volatile, no outputs, no memory
    /// clobber) asm when its results are unused.
    #[test]
    fn dce_removes_pure_asm_without_outputs() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let mut bb = BasicBlock {
            name: "entry".into(),
            phis: vec![],
            insts: vec![],
            term: Terminator::Ret { val: None },
            preds: vec![],
            succs: vec![],
        };
        // pure asm: no volatile, no outputs, no memory clobber
        bb.insts.push(Inst {
            dest: None,
            kind: InstKind::InlineAsm {
                asm_str: "nop".into(),
                outputs: vec![],
                output_names: vec![],
                output_constraints: vec![],
                output_vars: vec![],
                inputs: vec![],
                input_names: vec![],
                input_constraints: vec![],
                clobbers: vec![],
                volatile: false,
                ty: Type::Void,
            },
            span: span.clone(),
        });
        f.blocks.insert("entry".into(), bb);
        assert!(dead_inst_elim(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts.len(), 0, "pure asm should be eliminated");
    }

    /// DCE preserves volatile asm even without outputs.
    #[test]
    fn dce_preserves_volatile_asm() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let mut bb = BasicBlock {
            name: "entry".into(),
            phis: vec![],
            insts: vec![],
            term: Terminator::Ret { val: None },
            preds: vec![],
            succs: vec![],
        };
        bb.insts.push(Inst {
            dest: None,
            kind: InstKind::InlineAsm {
                asm_str: "nop".into(),
                outputs: vec![],
                output_names: vec![],
                output_constraints: vec![],
                output_vars: vec![],
                inputs: vec![],
                input_names: vec![],
                input_constraints: vec![],
                clobbers: vec![],
                volatile: true,
                ty: Type::Void,
            },
            span: span.clone(),
        });
        f.blocks.insert("entry".into(), bb);
        assert!(!dead_inst_elim(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts.len(), 1, "volatile asm must be preserved");
    }

    /// DCE preserves asm with memory clobber (observable side effect).
    #[test]
    fn dce_preserves_asm_with_memory_clobber() {
        use crate::ssa::*;
        let span = crate::diagnostics::Span::point("t.c", 1, 1);
        let mut f = Function::new("test".into(), Type::Int, span.clone());
        let mut bb = BasicBlock {
            name: "entry".into(),
            phis: vec![],
            insts: vec![],
            term: Terminator::Ret { val: None },
            preds: vec![],
            succs: vec![],
        };
        bb.insts.push(Inst {
            dest: None,
            kind: InstKind::InlineAsm {
                asm_str: "nop".into(),
                outputs: vec![],
                output_names: vec![],
                output_constraints: vec![],
                output_vars: vec![],
                inputs: vec![],
                input_names: vec![],
                input_constraints: vec![],
                clobbers: vec!["memory".into()],
                volatile: false,
                ty: Type::Void,
            },
            span: span.clone(),
        });
        f.blocks.insert("entry".into(), bb);
        assert!(!dead_inst_elim(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts.len(), 1, "asm with memory clobber must be preserved");
    }
    // ── P12b ubsan_ir_instrument tests ──

    /// 除/模可能陷阱: 上提后会跳过原本保护它的控制流, 整个块放弃。
    #[test]
    fn gvn_merges_duplicate_int_expression() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        let x = sv("x", 0, Type::Int);
        f.params.push(x.clone());
        let a = sv("_a", 1, Type::Int);
        let b = sv("_b", 2, Type::Int);
        let c = sv("_c", 0, Type::Int);
        let dup = |dest: SsaVar| Inst {
            dest: Some(dest),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(x.clone()),
                r: Operand::Int(1),
            },
            span: span.clone(),
        };
        f.blocks.get_mut("entry").unwrap().insts = vec![
            dup(a.clone()),
            dup(b.clone()),
            Inst {
                dest: Some(c.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(a),
                    r: Operand::Var(b),
                },
                span: span.clone(),
            },
        ];
        f.blocks.get_mut("entry").unwrap().term = Terminator::Ret { val: Some(Operand::Var(c)) };
        assert!(gvn(&mut f), "重复的 x+1 应被归一");
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(bb.insts.len(), 2, "第二条同形计算应被删除");
        // 归一后两个操作数指向同一个 SSA 名, 且该名仍有定义。
        let defined: Vec<String> =
            bb.insts.iter().filter_map(|i| i.dest.as_ref().map(|d| d.name())).collect();
        assert_eq!(defined, vec!["_a.1", "_c"], "保留的必须是先出现的那个定义");
        let uses_a = bb.insts[1]
            .kind
            .clone();
        match uses_a {
            InstKind::Bin { l, r, .. } => {
                assert_eq!(operand_identity(&l), "_a.1");
                assert_eq!(operand_identity(&r), "_a.1", "_b.2 必须被重命名成 _a.1");
            },
            _ => panic!("unexpected"),
        }
    }

    #[test]
    fn gvn_does_not_reuse_load_across_an_intervening_store() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        let p = sv("p", 0, Type::Ptr(Box::new(QualType::new(Type::Int))));
        f.params.push(p.clone());
        let addr = sv("_addr", 0, Type::Ptr(Box::new(QualType::new(Type::Int))));
        let l1 = sv("_l1", 0, Type::Int);
        let l2 = sv("_l2", 0, Type::Int);
        let bb = f.blocks.get_mut("entry").unwrap();
        bb.insts = vec![
            Inst {
                dest: Some(addr.clone()),
                kind: InstKind::AddrOf { base: "p".into() },
                span: span.clone(),
            },
            Inst {
                dest: Some(l1.clone()),
                kind: InstKind::Load {
                    e: Operand::Var(addr.clone()),
                    ty: Type::Int,
                    is_volatile: false,
                },
                span: span.clone(),
            },
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(addr.clone()),
                    src: Operand::Int(7),
                    is_volatile: false,
                },
                span: span.clone(),
            },
            Inst {
                dest: Some(l2.clone()),
                kind: InstKind::Load {
                    e: Operand::Var(addr.clone()),
                    ty: Type::Int,
                    is_volatile: false,
                },
                span: span.clone(),
            },
        ];
        bb.term = Terminator::Ret { val: Some(Operand::Var(l2)) };
        let before = f.blocks.get("entry").unwrap().insts.len();
        gvn(&mut f);
        let after = f.blocks.get("entry").unwrap().insts.len();
        assert_eq!(before, after, "中间隔着一次写 ⇒ 两个读不得归一");
    }

    #[test]
    fn gvn_respects_fenv_access_for_float_expressions() {
        let mk = || {
            let span = span();
            let mut f = Function::new("g".into(), Type::Double, span.clone());
            let x = sv("x", 0, Type::Double);
            f.params.push(x.clone());
            let a = sv("_a", 1, Type::Double);
            let b = sv("_b", 2, Type::Double);
            let dup = |dest: SsaVar| Inst {
                dest: Some(dest),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(x.clone()),
                    r: Operand::Float(crate::ast::FloatConst::zero(&Type::Double), Type::Double),
                },
                span: span.clone(),
            };
            f.blocks.get_mut("entry").unwrap().insts =
                vec![dup(a.clone()), dup(b.clone())];
            f.blocks.get_mut("entry").unwrap().term =
                Terminator::Ret { val: Some(Operand::Var(b)) };
            f
        };
        // FENV_ACCESS ON: 舍入模式/异常标志可被观测 ⇒ 浮点计算不得归一。
        let mut f = mk();
        f.fenv_access = true;
        gvn(&mut f);
        assert_eq!(
            f.blocks.get("entry").unwrap().insts.len(),
            2,
            "FENV_ACCESS ON 时两次浮点加法必须各自保留"
        );
        // FENV_ACCESS OFF: 同一块内无穿插写 ⇒ 允许归一。
        let mut g = mk();
        g.fenv_access = false;
        gvn(&mut g);
        assert_eq!(
            g.blocks.get("entry").unwrap().insts.len(),
            1,
            "默认 (FENV_ACCESS OFF) 下同块内的同形浮点加法可归一"
        );
    }

    #[test]
    fn gvn_collapses_degenerate_phi() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        let x = sv("x", 1, Type::Int);
        let p = sv("p", 0, Type::Int);
        f.params.push(p.clone());
        let merge = f.add_block("merge".into());
        // phi 的入边必须指向**真的有定义**的 SSA 名: 退化 = 把 dest 重命名到该入边名,
        // 没有定义的名字当不了改写目标。
        let q1 = y_var("q1");
        f.blocks.get_mut("entry").unwrap().insts = vec![Inst {
            dest: Some(q1.clone()),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Int(3),
                r: Operand::Int(0),
            },
            span: span.clone(),
        }];
        f.blocks.get_mut("entry").unwrap().term =
            Terminator::Branch {
                cond: Operand::Var(p),
                t: merge.clone(),
                f: merge.clone(),
            };
        f.blocks.get_mut(&merge).unwrap().phis = vec![crate::ssa::Phi {
            dest: x.clone(),
            incoming: vec![(q1.clone(), "entry".to_string()), (q1, "entry".to_string())],
        }];
        f.blocks.get_mut(&merge).unwrap().term = Terminator::Ret { val: Some(Operand::Var(x)) };
        assert!(gvn(&mut f), "所有入边同值的 phi 应退化");
        assert!(f.blocks.get(&merge).unwrap().phis.is_empty(), "退化后 phi 必须被删除");
    }

    #[test]
    fn dse_removes_overwritten_store_and_keeps_read_one() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        f.original_locals.insert("x".into());
        let addr = sv("_addr", 0, Type::Ptr(Box::new(QualType::new(Type::Int))));
        let keep = sv("_keep", 0, Type::Int);
        let bb = f.blocks.get_mut("entry").unwrap();
        bb.insts = vec![
            Inst {
                dest: Some(addr.clone()),
                kind: InstKind::AddrOf { base: "x".into() },
                span: span.clone(),
            },
            // 第一次写: 后来被同位置写覆盖, 中间无人读 ⇒ 死
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(addr.clone()),
                    src: Operand::Int(1),
                    is_volatile: false,
                },
                span: span.clone(),
            },
            // 第二次写: 后面有读 ⇒ 必须保留
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(addr.clone()),
                    src: Operand::Int(2),
                    is_volatile: false,
                },
                span: span.clone(),
            },
            Inst {
                dest: Some(keep.clone()),
                kind: InstKind::Load {
                    e: Operand::Var(addr),
                    ty: Type::Int,
                    is_volatile: false,
                },
                span: span.clone(),
            },
        ];
        bb.term = Terminator::Ret { val: Some(Operand::Var(keep)) };
        assert!(dead_store_elim(&mut f), "被覆盖且未被读的写应被删除");
        let bb = f.blocks.get("entry").unwrap();
        let stores: Vec<&Inst> = bb
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::Store { .. }))
            .collect();
        assert_eq!(stores.len(), 1, "只应留下被读到的那次写");
        match &stores[0].kind {
            InstKind::Store { src, .. } => assert!(matches!(src, Operand::Int(2))),
            _ => unreachable!(),
        }
    }

    #[test]
    fn dse_keeps_volatile_and_global_stores() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        f.original_locals.insert("x".into());
        let addr = sv("_addr", 0, Type::Ptr(Box::new(QualType::new(Type::Int))));
        let gaddr = sv("_gaddr", 0, Type::Ptr(Box::new(QualType::new(Type::Int))));
        let bb = f.blocks.get_mut("entry").unwrap();
        bb.insts = vec![
            Inst {
                dest: Some(addr.clone()),
                kind: InstKind::AddrOf { base: "x".into() },
                span: span.clone(),
            },
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(addr.clone()),
                    src: Operand::Int(1),
                    is_volatile: true,
                },
                span: span.clone(),
            },
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(addr.clone()),
                    src: Operand::Int(2),
                    is_volatile: false,
                },
                span: span.clone(),
            },
            // 全局对象 (不在 original_locals 里): 其他 TU / 被调者可读 ⇒ 不删
            Inst {
                dest: Some(gaddr.clone()),
                kind: InstKind::AddrOf { base: "g".into() },
                span: span.clone(),
            },
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(gaddr.clone()),
                    src: Operand::Int(1),
                    is_volatile: false,
                },
                span: span.clone(),
            },
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(gaddr.clone()),
                    src: Operand::Int(2),
                    is_volatile: false,
                },
                span: span.clone(),
            },
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::Var(addr.clone()),
                    src: Operand::Int(3),
                    is_volatile: false,
                },
                span: span.clone(),
            },
        ];
        bb.term = Terminator::Ret { val: None };
        dead_store_elim(&mut f);
        // 把剩下的写逐条列出比较 —— 只数个数无法区分「volatile 被删」与「全局被删」,
        // 而这两类的严重性不同。
        let remaining: Vec<(String, String, bool)> = f
            .blocks
            .get("entry")
            .unwrap()
            .insts
            .iter()
            .filter_map(|i| match &i.kind {
                InstKind::Store { e, src, is_volatile } => Some((
                    format!("{:?}", access_loc(e, &collect_defs(&f))),
                    format!("{src:?}"),
                    *is_volatile,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            remaining,
            vec![
                // volatile 写: 可观测副作用, 无条件保留
                ("Named(\"x\")".to_string(), "Int(1)".to_string(), true),
                // 全局对象的两次写: 其他 TU / 被调者可读, 一律保留
                ("Named(\"g\")".to_string(), "Int(1)".to_string(), false),
                ("Named(\"g\")".to_string(), "Int(2)".to_string(), false),
            ],
            "DSE 只应删掉局部对象上「被后续同位置写覆盖」的那两次写"
        );
    }

    #[test]
    fn jump_threading_folds_constant_branch_and_prunes_phi() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        let taken = f.add_block("taken".into());
        let not_taken = f.add_block("not_taken".into());
        let merged = f.add_block("merged".into());
        f.blocks.get_mut("entry").unwrap().term = Terminator::Branch {
            cond: Operand::Int(1),
            t: taken.clone(),
            f: not_taken.clone(),
        };
        f.blocks.get_mut(&taken).unwrap().term = Terminator::Jump(merged.clone());
        f.blocks.get_mut(&not_taken).unwrap().term = Terminator::Jump(merged.clone());
        let z = sv("z", 1, Type::Int);
        f.blocks.get_mut(&merged).unwrap().phis = vec![crate::ssa::Phi {
            dest: z.clone(),
            incoming: vec![(y_var("a"), taken.clone()), (y_var("b"), not_taken.clone())],
        }];
        f.blocks.get_mut(&merged).unwrap().term = Terminator::Ret { val: Some(Operand::Var(z)) };
        assert!(jump_threading(&mut f), "常量分支应被折叠成无条件跳");
        // 折叠后 `entry → taken → merged` 又是一条空跳板链, 同轮里的链合并会再推一步:
        // 这里只要求「不再是 Branch」且「不得再走到 not_taken」。
        match &f.blocks.get("entry").unwrap().term {
            Terminator::Jump(t) => assert_ne!(t, &not_taken, "折叠后的跳转不得指向未走到的分支"),
            other => panic!("cond=1 的分支必须被折叠成 Jump, 实际 {other:?}"),
        }
        // 不可达的 not_taken 被剔除; merged 的 phi 只剩一个入边 ⇒ 退化。
        assert!(!f.blocks.contains_key(&not_taken), "未走到的块应成为不可达而被剔除");
        assert!(
            f.blocks.get(&merged).unwrap().phis.is_empty(),
            "只剩一个入边的 phi 必须退化"
        );
    }

    #[test]
    fn jump_threading_merges_empty_trampoline_block() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        let mid = f.add_block("mid".into());
        let end = f.add_block("end".into());
        f.blocks.get_mut("entry").unwrap().term = Terminator::Jump(mid.clone());
        f.blocks.get_mut(&mid).unwrap().term = Terminator::Jump(end.clone());
        f.blocks.get_mut(&end).unwrap().term = Terminator::Ret { val: Some(Operand::Int(0)) };
        assert!(jump_threading(&mut f), "空跳板块应被合并掉");
        assert!(
            matches!(&f.blocks.get("entry").unwrap().term, Terminator::Jump(t) if t == &end),
            "entry 应直接跳到最终目标"
        );
        assert!(!f.blocks.contains_key(&mid), "空跳板块应不可达而被剔除");
    }

    /// 本轮 P14 根因的回归守卫: 上提必须沿依赖关包络, 不得按 dest 名字字典序重排。
    #[test]
    fn speculative_execution_never_moves_a_use_before_its_def() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        let s1 = sv("s", 1, Type::Int);
        let t1 = sv("t", 1, Type::Int);
        let s2 = sv("s", 2, Type::Int);
        let gv = sv("gv", 0, Type::Int);
        let k = sv("k", 0, Type::Int);
        f.params.push(gv.clone());
        f.params.push(k.clone());
        f.original_locals.insert("s".into());
        f.blocks.get_mut("entry").unwrap().insts = vec![
            Inst {
                dest: Some(s1.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Int(0),
                    r: Operand::Int(0),
                },
                span: span.clone(),
            },
            Inst {
                dest: Some(t1.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(gv),
                    r: Operand::Int(7),
                },
                span: span.clone(),
            },
            // `s = s + t` 的 dest 基名是 `s`, 字典序小于 `t` —— 旧实现会把它排到
            // t 的定义之前, 重建后 `t` 的使用跑在赋值之前。
            Inst {
                dest: Some(s2.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: Operand::Var(s1),
                    r: Operand::Var(t1.clone()),
                },
                span: span.clone(),
            },
        ];
        f.blocks.get_mut("entry").unwrap().term =
            Terminator::Ret { val: Some(Operand::Var(s2)) };
        let def_names = |f: &Function| -> Vec<String> {
            f.blocks
                .get("entry")
                .unwrap()
                .insts
                .iter()
                .map(|i| i.dest.as_ref().map(|d| d.name()).unwrap_or_default())
                .collect()
        };
        let before = def_names(&f);
        speculative_execution(&mut f);
        let after = def_names(&f);
        assert_eq!(before, after, "无分支可提前时不得重排任何指令");
        assert_uses_follow_defs(&f);
    }

    /// `dead_inst_elim` 的守卫: 具名局部变量的赋值不得因「本层无 use」而删。
    #[test]
    fn dead_inst_elim_keeps_named_local_initializers() {
        let span = span();
        let mut f = Function::new("g".into(), Type::Int, span.clone());
        f.original_locals.insert("g".into());
        let g1 = sv("g", 1, Type::Ptr(Box::new(QualType::new(Type::Int))));
        let bb = f.blocks.get_mut("entry").unwrap();
        bb.insts = vec![Inst {
            dest: Some(g1),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::SymAddr(
                    "add".into(),
                    Type::Ptr(Box::new(QualType::new(Type::Int))),
                ),
                r: Operand::Int(0),
            },
            span: span.clone(),
        }];
        bb.term = Terminator::Ret { val: None };
        dead_inst_elim(&mut f);
        assert_eq!(
            f.blocks.get("entry").unwrap().insts.len(),
            1,
            "变量 `g` 的初始化必须保留: 删它等于让那个栈槽从未被写入"
        );
    }

    fn sv(base: &str, version: u32, ty: Type) -> SsaVar {
        SsaVar {
            base: base.into(),
            version,
            ty,
            span: span(),
        }
    }

    fn y_var(base: &str) -> SsaVar {
        sv(base, 1, Type::Int)
    }


    /// 断言一个块内不存在「使用早于定义」的序列 (调度类 pass 的通用不变式)。
    /// 参数名的定义在函数入口之前, 因此预先计入已定义集。
    fn assert_uses_follow_defs(f: &Function) {
        let mut defined: HashSet<String> =
            f.params.iter().map(|p| p.name()).collect();
        let bb = f.blocks.get("entry").unwrap();
        for inst in &bb.insts {
            for u in operand_uses(&inst.kind) {
                assert!(
                    defined.contains(&u),
                    "指令使用了尚未定义的 SSA 名 {u}: 调度破坏了依赖顺序"
                );
            }
            if let Some(d) = &inst.dest {
                defined.insert(d.name());
            }
        }
    }

    /// 循环 IR 跑完整条管线后的**结构不变式**。
    ///
    /// 含循环的函数现在无条件走 SSA 管线，本测试又直接调用 build + pipeline + rebuild，
    /// 所以它与「准入闸门」完全无关：即使今后某条准入判据把这类函数挡回 AST 路径，
    /// 这里仍然守住在 SSA 层必须成立的不变式。它的价值在于把这类检查从「行为对照
    /// 能不能碰上这条路径」解耦出来（本轮解锁过程中，GVN 造出悬空引用与 phi 不覆盖
    /// 体内使用两个真缺陷就是先由这类结构检查抢到的）。
    #[test]
    fn loop_ir_pipeline_keeps_ssa_invariants() {
        let src = "int dw(int n){int s=0;do{s+=n;n--;}while(n>0);return s;}\n\
                   int nest(int n){int s=0;for(int i=0;i<n;i++)for(int j=0;j<n;j++)s+=1;return s;}\n\
                   int cont(int n){int s=0;for(int i=0;i<n;i++){if(i==1)continue;s+=i;}return s;}\n\
                   int brk(int n){int s=0;for(int i=0;i<n;i++){if(i==3)break;s+=i;}return s;}\n\
                   int wh(int n){int s=0;while(n>0){s+=n;n--;}return s;}\n\
                   int carry(int n){int s=0;int m=3;for(int i=0;i<n;i++){s+=m*i;m=m+1;}return s;}\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let mut checked = 0;
        for decl in m.source().decls.iter() {
            let crate::ast::Decl::Func(f) = decl else { continue };
            let Some(ssa) = crate::ssa::build_from_func(f) else {
                continue;
            };
            // 进入管线前: 循环携带变量必须有 phi (体内使用已重命名到 phi dest)。
            let phi_bases: HashSet<String> = ssa
                .blocks
                .values()
                .flat_map(|b| b.phis.iter().map(|p| p.dest.base.clone()))
                .collect();
            if matches!(
                f.name.name.as_str(),
                "wh" | "dw" | "nest" | "cont" | "brk" | "carry"
            ) {
                assert!(
                    phi_bases.contains("s") || phi_bases.contains("i") || phi_bases.contains("n"),
                    "{}: 循环携带变量没有任何 phi ⇒ IR 不在 SSA 形式上",
                    f.name.name
                );
            }
            for level in [2u8, 3u8] {
                // 整函数复制走显式 `fork()`：pass 要在副本上跑，位置表也必须一起带走。
                let mut s = ssa.fork();
                run_with_opts(&mut s, level, true);
                let dangling = crate::ssa::dangling_refs(&s);
                assert!(
                    dangling.is_empty(),
                    "{} @-O{}: 管线造出悬空 SSA 引用 {:?}",
                    f.name.name,
                    level,
                    dangling
                );
                // 单赋值必须成立: 否则重建按基名塌缩后会把副本**再执行一遍**。
                let dup = crate::ssa::duplicate_ssa_defs(&s);
                assert!(
                    dup.is_empty(),
                    "{} @-O{}: 管线破坏了单赋值 (重复定义 {:?}) ⇒ 重建会重算一遍",
                    f.name.name,
                    level,
                    dup
                );
                // 重建必须可完成且不 panic (内部一致性)。
                let _ = crate::ssa::rebuild_block(&s);
            }
            checked += 1;
        }
        assert_eq!(checked, 6, "6 个循环形态都应能 SSA 化");
    }

    /// 负向对照：上面那条单赋值不变式不得是空判。
    ///
    /// 把一条已有指令**原样追加一遍**（正是已删的伪 `loop_tiling` 的作法形状），
    /// 检查器必须报出那个重复定义的名字。
    #[test]
    fn duplicate_ssa_defs_detects_appended_copy() {
        let src = "int wh(int n){int s=0;while(n>0){s+=n;n--;}return s;}\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let f = m
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                crate::ast::Decl::Func(f) => Some(f),
                _ => None,
            })
            .expect("wh");
        let mut s = crate::ssa::build_from_func(f).expect("wh 应能 SSA 化");
        assert!(
            crate::ssa::duplicate_ssa_defs(&s).is_empty(),
            "刚构建的 IR 应满足单赋值: {:?}",
            crate::ssa::duplicate_ssa_defs(&s)
        );
        // 选一条有 dest 的指令, 把它追到同一个块末尾。
        let bb_name = s
            .blocks
            .iter()
            .find(|(_, b)| b.insts.iter().any(|i| i.dest.is_some()))
            .map(|(k, _)| k.clone())
            .expect("应至少有一条带 dest 的指令");
        let (dup, idx) = {
            let b = s.blocks.get(&bb_name).unwrap();
            let i = b.insts.iter().position(|x| x.dest.is_some()).unwrap();
            (b.insts[i].clone(), i)
        };
        let name = dup.dest.as_ref().unwrap().name();
        s.blocks.get_mut(&bb_name).unwrap().insts.push(dup);
        let found = crate::ssa::duplicate_ssa_defs(&s);
        assert_eq!(
            found,
            vec![name.clone()],
            "重复定义 {name} 必须被抓到 (idx={idx}, bb={bb_name})"
        );
    }

    #[test]
    fn ubsan_ir_div_inserts_guard() {
        let span = Span::point("test.c", 1, 1);
        let mut f = Function::new("foo".into(), Type::Int, span.clone());
        f.params.push(SsaVar {
            base: "a".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        });
        f.params.push(SsaVar {
            base: "b".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        });
        let bb = f.blocks.get_mut("entry").unwrap();
        // t = a / b (should trigger divrem_overflow guard)
        bb.insts.push(Inst {
            dest: Some(SsaVar {
                base: "t".into(),
                version: 1,
                ty: Type::Int,
                span: span.clone(),
            }),
            kind: InstKind::Bin {
                op: BinOp::Div,
                l: Operand::Var(f.params[0].clone()),
                r: Operand::Var(f.params[1].clone()),
            },
            span: span.clone(),
        });
        assert!(ubsan_ir_instrument(&mut f));
        let entry = f.blocks.get("entry").unwrap();
        // Should have 2 instructions: the div + the guard
        assert_eq!(entry.insts.len(), 2);
        match &entry.insts[1].kind {
            InstKind::Store {
                e: Operand::SymAddr(name, _),
                ..
            } => {
                assert!(name.contains("divrem_overflow"), "got: {}", name);
            },
            other => panic!("expected ubsan guard, got {:?}", other),
        }
    }

    #[test]
    fn ubsan_ir_const_nonzero_skips_guard() {
        let span = Span::point("test.c", 1, 1);
        let mut f = Function::new("bar".into(), Type::Int, span.clone());
        f.params.push(SsaVar {
            base: "a".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        });
        let bb = f.blocks.get_mut("entry").unwrap();
        // t = a / 5 (constant non-zero divisor, no guard needed)
        bb.insts.push(Inst {
            dest: Some(SsaVar {
                base: "t".into(),
                version: 1,
                ty: Type::Int,
                span: span.clone(),
            }),
            kind: InstKind::Bin {
                op: BinOp::Div,
                l: Operand::Var(f.params[0].clone()),
                r: Operand::Int(5),
            },
            span: span.clone(),
        });
        assert!(!ubsan_ir_instrument(&mut f));
        let entry = f.blocks.get("entry").unwrap();
        assert_eq!(entry.insts.len(), 1, "no guard should be inserted");
    }

    #[test]
    fn ubsan_ir_negate_inserts_guard() {
        let span = Span::point("test.c", 1, 1);
        let mut f = Function::new("neg".into(), Type::Int, span.clone());
        f.params.push(SsaVar {
            base: "x".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        });
        let bb = f.blocks.get_mut("entry").unwrap();
        // t = -x (should trigger negate_overflow guard)
        bb.insts.push(Inst {
            dest: Some(SsaVar {
                base: "t".into(),
                version: 1,
                ty: Type::Int,
                span: span.clone(),
            }),
            kind: InstKind::Un {
                op: UnOp::Neg,
                e: Operand::Var(f.params[0].clone()),
            },
            span: span.clone(),
        });
        assert!(ubsan_ir_instrument(&mut f));
        let entry = f.blocks.get("entry").unwrap();
        assert_eq!(entry.insts.len(), 2);
        match &entry.insts[1].kind {
            InstKind::Store {
                e: Operand::SymAddr(name, _),
                ..
            } => {
                assert!(name.contains("negate_overflow"), "got: {}", name);
            },
            other => panic!("expected ubsan guard, got {:?}", other),
        }
    }

    /// SCCP 的常量表是无类型的 i128，因此替常量**不得改变操作数的类型类**。
    ///
    /// 旧行为把 `(long double)50` 登成整数 50，变参调用的实参从 `Var( _: LongDouble)`
    /// 变成 `Int(50)`：数值看着一样，ABI 类却从 MEMORY 换成 INTEGER，caller 不再预留
    /// 16 字节溢出区 ⇒ callee 侧 `va_arg(long double)` 全部读错位置（实测四个 va_arg
    /// 夹具同时红）。反向对照：整数实参仍必须被折叠，否则这条判据只是「关掉 SCCP」。
    #[test]
    fn sccp_const_fold_keeps_the_operand_type_class() {
        let src = "int g(long double); int h(int);\
                    int f(void){ long double d = (long double)50; int n = 7 + 3;\
                          return g(d) + h(n); }\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        for decl in m.source().decls.iter() {
            if let Decl::Func(fh) = decl {
                if fh.name.name != "f" {
                    continue;
                }
                let mut ssa = crate::ssa::build_from_func(fh).expect("应能 SSA 化");
                sparse_conditional_constant_propagation(&mut ssa);
                let mut ld_arg: Option<Operand> = None;
                let mut int_arg: Option<Operand> = None;
                for b in ssa.blocks.values() {
                    for i in &b.insts {
                        if let InstKind::Call { callee, args, .. } = &i.kind {
                            match callee {
                                Operand::SymAddr(n, _) if n == "g" => ld_arg = args.first().cloned(),
                                Operand::SymAddr(n, _) if n == "h" => int_arg = args.first().cloned(),
                                _ => {}
                            }
                        }
                    }
                }
                let ld = ld_arg.expect("应有对 g 的调用");
                assert!(
                    matches!(&ld, Operand::Var(v) if matches!(v.ty, Type::LongDouble))
                        || matches!(&ld, Operand::Float(..)),
                    "long double 实参不得被换成整数常量：{ld:?}"
                );
                let it = int_arg.expect("应有对 h 的调用");
                assert!(
                    matches!(&it, Operand::Int(10)),
                    "整数常量传播必须仍然工作（否则本判据只是在关掉 SCCP）：{it:?}"
                );
                return;
            }
        }
        panic!("找不到函数 f");
    }
}
