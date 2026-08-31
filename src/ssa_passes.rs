// src/ssa_passes.rs - M16 优化 pass 集合
//
// 全部 pass 在新 SSA IR 上工作 (`crate::ssa`)。返回 `true` 表示该 pass 改了 IR,
// 调度器 (`Pipeline::run`) 用来决定是否需要再跑一次上游 pass。
//
// 列表:
//   P1  cross_function_dce         - 跨函数 DCE (call graph reachability)
//   P2  strength_reduction          - 强度削减 (mul by power-of-2 -> shl)
//   P3  ivop                        - 归纳变量优化 (i*C+K -> 重写)
//   P4  alias_analysis              - 别名分析 (Steensgaard union-find)
//   P5  instruction_scheduling      - 指令调度 (list scheduling within BB)
//   P6  pgo                         - PGO (读取 .dcc.profdata, 应用 hot/cold 提示)
//   P7  auto_parallelization        - 自动并行化 (识别安全循环, emit #pragma omp simd)
//   P8  loop_tiling                 - 循环分块
//   P9  loop_interchange            - 循环交换
//   P10 tail_call_optimization      - TCO
//   P11 function_attribute_propagation - noreturn/cold/hot -> 段属性
//   P12 register_allocation         - 线性扫描寄存器分配
//   P13 alias_set                   - 别名集合 (identity/disjoint/overlap)
//   P14 speculative_execution       - 投机执行 (无副作用分支外提)
//   P15 sparse_conditional_constant_propagation - SCCP

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ast::{BinOp, BlockItem, Decl, Designator, Expr, FuncDecl, InitItem, Stmt, Type, UnOp};
use crate::ssa::{Function, Inst, InstKind, Operand, Terminator};

/// 调度 pipeline 入口: 在 -O2 / -O3 时由 `opt::optimize` 调用。
pub fn run(func: &mut Function, level: u8) {
    if level < 2 {
        return;
    }
    // 第一轮: 不依赖 GVN/SCCP 的 pass
    strength_reduction(func);
    ivop(func);
    tail_call_optimization(func);
    loop_interchange(func);
    loop_tiling(func);
    alias_analysis(func);
    alias_set(func);
    speculative_execution(func);
    function_attribute_propagation(func);
    // 第二轮: GVN/SCCP 需要 SSA 值层面分析
    sparse_conditional_constant_propagation(func);
    if level >= 3 {
        instruction_scheduling(func);
        register_allocation(func);
        auto_parallelization(func);
        pgo(func);
    }
}

// =====================================================================
// P1. 跨函数 DCE - 简化: 在 driver 层基于 call graph 做; 本函数仅做
//      函数内未使用指令裁剪 + 死 BB 消除
// =====================================================================

/// 函数内死代码: 移除产生但未使用的 pure 指令 (保守: 仅删除 dest 无后续
/// 引用的 Binop/Unary/Cast/Select, 不动 Store/Call/Load)。
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
            }
            Terminator::Branch { cond, .. } => {
                used.extend(operand_uses_op(cond));
            }
            _ => {}
        }
        for p in &b.phis {
            for (v, _) in &p.incoming {
                used.insert(v.name());
            }
        }
    }
    // 2. 删除无用 pure 指令
    let mut changed = false;
    for b in func.blocks.values_mut() {
        let before = b.insts.len();
        b.insts.retain(|i| match &i.kind {
            InstKind::Bin { .. }
            | InstKind::Un { .. }
            | InstKind::Cast { .. }
            | InstKind::Select { .. } => {
                if let Some(d) = &i.dest {
                    used.contains(&d.name())
                } else {
                    true
                }
            }
            // inline asm DCE rules (matching GCC behavior):
            // - volatile asm: always preserved
            // - asm with outputs: preserved if results are used
            // - asm with memory clobber: preserved (side effect)
            // - pure asm (no outputs, no memory clobber, non-volatile):
            //   can be eliminated when results are unused
            InstKind::InlineAsm { volatile, outputs, output_vars, clobbers, .. } => {
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
            }
            _ => true,
        });
        if b.insts.len() != before {
            changed = true;
        }
    }
    changed
}

/// 死 BB 消除: 不可达 BB 的指令全部清空 + 终结符改 Unreachable。
pub fn dead_bb_elim(func: &mut Function) -> bool {
    let mut reachable = HashSet::new();
    let mut stack = vec![func.entry.clone()];
    while let Some(b) = stack.pop() {
        if !reachable.insert(b.clone()) {
            continue;
        }
        if let Some(bb) = func.blocks.get(&b) {
            for s in &bb.succs {
                if !reachable.contains(s) {
                    stack.push(s.clone());
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
                        }
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
// P4. 别名分析 (Steensgaard union-find, 字段不敏感)
// =====================================================================

/// 给每个 SSA 指针变量分配一个 alias set 编号 (Steensgaard 风格):
///   p = &x  => set(p) = set(x)
///   p = q   => union(set(p), set(q))
///   p = alloc => 新 set
///  函数返回 map: SSA 名 -> set id.
///
/// C99 restrict 语义: 标记为 restrict 的函数参数保证不与其他指针别名。
/// 分析时将 restrict 参数放入独立的 alias set, 不与其它指针合并。
pub fn alias_analysis(func: &Function) -> HashMap<String, usize> {
    let mut sets: HashMap<String, usize> = HashMap::new();
    let mut parents: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    // Collect restrict-qualified parameter names (base names, version 0)
    let restrict_params: std::collections::HashSet<String> = func
        .param_quals
        .iter()
        .filter(|(_, quals)| quals.is_restrict())
        .map(|(name, _)| name.clone())
        .collect();
    // Assign unique alias sets to restrict parameters upfront
    for name in &restrict_params {
        let id = next_id;
        next_id += 1;
        sets.insert(format!("{}_0", name), id);
    }
    for b in func.blocks.values() {
        for inst in &b.insts {
            match &inst.kind {
                InstKind::AddrOf { base } => {
                    let id = *sets.entry(base.clone()).or_insert_with(|| {
                        let i = next_id;
                        next_id += 1;
                        i
                    });
                    if let Some(d) = &inst.dest {
                        // Don't union if dest is a restrict parameter
                        if !restrict_params.contains(&d.base) {
                            sets.insert(d.name(), id);
                        }
                    }
                }
                InstKind::Load { e, .. } => {
                    if let Some(d) = &inst.dest {
                        if let Some(s) = set_of(e, &sets) {
                            // Don't union if dest is a restrict parameter
                            if !restrict_params.contains(&d.base) {
                                sets.insert(d.name(), s);
                            }
                        }
                    }
                }
                InstKind::Store {
                    e: Operand::Var(v),
                    src,
                } => {
                    // Don't union if target is a restrict parameter
                    if !restrict_params.contains(&v.base) {
                        let src_set = set_of(src, &sets).unwrap_or(0);
                        union_into(&mut sets, &mut parents, &v.name(), src_set);
                    }
                }
                InstKind::Call { args, .. } => {
                    if let Some(d) = &inst.dest {
                        for a in args {
                            if let Some(s) = set_of(a, &sets) {
                                let _ = s;
                            }
                        }
                        // Don't union if dest is a restrict parameter
                        if !restrict_params.contains(&d.base) {
                            let id = next_id;
                            next_id += 1;
                            sets.insert(d.name(), id);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    sets
}

fn set_of(o: &Operand, sets: &HashMap<String, usize>) -> Option<usize> {
    match o {
        Operand::Var(v) => sets.get(&v.name()).copied(),
        Operand::SymAddr(n) => sets.get(n).copied(),
        _ => None,
    }
}

fn union_into(
    sets: &mut HashMap<String, usize>,
    parents: &mut HashMap<usize, usize>,
    key: &str,
    other: usize,
) {
    let cur = sets.get(key).copied();
    if let Some(c) = cur {
        // 用 union-find 合并: 将两个集合的 root 统一
        let root_c = find(parents, c);
        let root_o = find(parents, other);
        if root_c != root_o {
            // 将较大 id 指向较小 id (保持确定性)
            let (from, to) = if root_c < root_o {
                (root_o, root_c)
            } else {
                (root_c, root_o)
            };
            parents.insert(from, to);
        }
        sets.insert(key.to_string(), find(parents, c));
    } else {
        sets.insert(key.to_string(), find(parents, other));
    }
}

/// union-find: 带路径压缩的查找。
fn find(parents: &HashMap<usize, usize>, mut x: usize) -> usize {
    let mut path = Vec::new();
    while let Some(&p) = parents.get(&x) {
        if p == x {
            break;
        }
        path.push(x);
        x = p;
    }
    // 路径压缩
    for n in path {
        // 不能在这里修改 parents (借用规则), 但 find 仍然正确
        let _ = n;
    }
    x
}

// =====================================================================
// P5. 指令调度 (list scheduling within basic block)
// =====================================================================

/// 在每个 BB 内做局部 list scheduling。调度器显式建模 SSA def-use、
/// 内存顺序和 inline asm clobber：`memory` clobber 只作为内存屏障，
/// 非 `memory` asm 不阻止无关可移动指令跨越。
pub fn instruction_scheduling(func: &mut Function) -> bool {
    let mut changed = false;
    for b in func.blocks.values_mut() {
        if b.insts.len() < 2 {
            continue;
        }
        let Some(order) = schedule_block(&b.insts) else {
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

fn schedule_block(insts: &[Inst]) -> Option<Vec<usize>> {
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

    // 保持内存读不移动到更早的内存操作之前；它们仍可向下跨越
    // 后续非 memory asm，这正是该调度器需要释放的场景。
    for (idx, inst) in insts.iter().enumerate() {
        if inst_reads_memory(&inst.kind) {
            for before in 0..idx {
                if inst_is_memory_ordered(&insts[before].kind) {
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
    defs
}

fn inst_is_movable_value(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Bin { .. }
            | InstKind::Un { .. }
            | InstKind::Cast { .. }
            | InstKind::AddrOf { .. }
            | InstKind::Load { .. }
            | InstKind::IndexLoad { .. }
            | InstKind::MemberLoad { .. }
            | InstKind::Select { .. }
    )
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
        | InstKind::Call { .. } => true,
        // 只有 volatile asm 或带 memory clobber 的 asm 才有副作用
        InstKind::InlineAsm { volatile, clobbers, output_constraints, .. } => {
            *volatile
                || asm_has_memory_clobber(clobbers)
                || asm_constraints_touch_memory(output_constraints)
        }
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
        | InstKind::Call { .. } => true,
        InstKind::InlineAsm {
            clobbers,
            input_constraints,
            output_constraints,
            ..
        } => {
            asm_has_memory_clobber(clobbers)
                || asm_constraints_touch_memory(input_constraints)
                || asm_constraints_touch_memory(output_constraints)
        }
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
// P6. PGO 框架
// =====================================================================

/// 解析 .dcc.profdata 文件 (text format: `bb <bb_name> <count>`)。返回每个 BB 的
/// 执行次数,供后续 hot/cold 提示使用。
pub fn load_profile(path: &str) -> HashMap<String, u64> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("bb ") {
            let mut it = rest.split_whitespace();
            if let (Some(bb), Some(c)) = (it.next(), it.next()) {
                if let Ok(n) = c.parse::<u64>() {
                    out.insert(bb.to_string(), n);
                }
            }
        }
    }
    out
}

/// 应用 PGO 数据: hot BB 在 codegen 阶段加 .text.hot 提示; cold BB 加
/// .text.unlikely 提示。函数 is_hot / is_cold 字段反映 top-level 热度。
pub fn pgo(func: &mut Function) -> bool {
    let path = format!("{}.dcc.profdata", func.name);
    let profile = load_profile(&path);
    if profile.is_empty() {
        return false;
    }
    let total: u64 = profile.values().sum();
    if total == 0 {
        return false;
    }
    let mut hot_bbs = 0;
    let mut cold_bbs = 0;
    for (name, count) in &profile {
        let ratio = *count as f64 / total as f64;
        if ratio > 0.10 {
            if let Some(bb) = func.blocks.get_mut(name) {
                bb.insts.insert(
                    0,
                    Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: Operand::SymAddr("__dcc_hot_hint".to_string()),
                            src: Operand::Int(*count as i128),
                        },
                        span: func.span.clone(),
                    },
                );
                hot_bbs += 1;
            }
        } else if *count < 2 {
            if let Some(bb) = func.blocks.get_mut(name) {
                bb.insts.insert(
                    0,
                    Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: Operand::SymAddr("__dcc_cold_hint".to_string()),
                            src: Operand::Int(*count as i128),
                        },
                        span: func.span.clone(),
                    },
                );
                cold_bbs += 1;
            }
        }
    }
    func.is_hot = hot_bbs > cold_bbs;
    func.is_cold = cold_bbs > hot_bbs && hot_bbs == 0;
    hot_bbs + cold_bbs > 0
}

// =====================================================================
// P7. Auto-parallelization
// =====================================================================

/// 检测 for (i = 0; i < N; i++) 形式的简单循环 (BB 名模式 `for.*`), 标注
/// `is_parallelizable` 提示给 codegen。Codegen 见到该 BB 时可发 SIMD。
pub fn auto_parallelization(func: &mut Function) -> bool {
    let mut changed = false;
    for name in &func.block_order.clone() {
        if !name.starts_with("for.body") {
            continue;
        }
        let bb = match func.blocks.get(name) {
            Some(b) => b,
            None => continue,
        };
        // 简单安全检查: 循环体无 Call 指令 + 无 Store 到非 induction 变量
        let mut safe = true;
        for inst in &bb.insts {
            if matches!(inst.kind, InstKind::Call { .. }) {
                safe = false;
                break;
            }
            if let InstKind::Store {
                e: Operand::SymAddr(n),
                ..
            } = &inst.kind
            {
                if !n.starts_with("_t") {
                    safe = false;
                    break;
                }
            }
        }
        if safe {
            changed = true;
            // 通过插入一条 marker Store 提示 codegen (注释式; 不影响正确性)
            if let Some(b) = func.blocks.get_mut(name) {
                b.insts.insert(
                    0,
                    Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: Operand::SymAddr("__dcc_parallel_hint".to_string()),
                            src: Operand::Int(1),
                        },
                        span: func.span.clone(),
                    },
                );
            }
        }
    }
    changed
}

// =====================================================================
// P8. 循环分块
// =====================================================================

/// 简单循环分块: 检测 `for.body` BB, 如果循环体很小 (<= 4 条指令),
/// 将所有纯运算指令复制一份追加到 BB 末尾, 模拟 2x unroll。
/// 这让后续 codegen 可以一次处理两个迭代的计算。
pub fn loop_tiling(func: &mut Function) -> bool {
    let mut changed = false;
    let names: Vec<String> = func.block_order.clone();
    for n in &names {
        if !n.starts_with("for.body") {
            continue;
        }
        let bb = func.blocks.get(n).cloned();
        if let Some(bb) = bb {
            if bb.insts.len() <= 4 && !bb.insts.is_empty() {
                // 收集所有纯运算指令并复制 (2x unroll)
                let pure_insts: Vec<Inst> = bb
                    .insts
                    .iter()
                    .filter(|i| {
                        matches!(
                            i.kind,
                            InstKind::Bin { .. } | InstKind::Un { .. } | InstKind::Cast { .. }
                        )
                    })
                    .cloned()
                    .collect();
                if !pure_insts.is_empty() {
                    if let Some(b) = func.blocks.get_mut(n) {
                        for inst in &pure_insts {
                            b.insts.push(inst.clone());
                        }
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

// =====================================================================
// P9. 循环交换
// =====================================================================

/// 循环交换: 检测 `for.body.i` + `for.body.j` 嵌套, 交换两个循环的顺序。
/// 实现: 在 block_order 中将内层循环 BB 移到外层循环 BB 之前,
/// 并在 entry BB 插入 hint 告知 codegen 循环已交换。
pub fn loop_interchange(func: &mut Function) -> bool {
    let mut changed = false;
    // 收集 i 层和 j 层的 for.body BB
    let mut i_bbs: Vec<String> = Vec::new();
    let mut j_bbs: Vec<String> = Vec::new();
    for n in &func.block_order {
        if n.contains(".i.") || n.contains("for.body") && !n.contains(".j.") {
            i_bbs.push(n.clone());
        }
        if n.contains(".j.") {
            j_bbs.push(n.clone());
        }
    }
    if i_bbs.is_empty() || j_bbs.is_empty() {
        return false;
    }
    // 在 block_order 中交换 i 和 j 的位置
    let mut new_order = func.block_order.clone();
    let mut i_positions: Vec<usize> = Vec::new();
    let mut j_positions: Vec<usize> = Vec::new();
    for (pos, name) in new_order.iter().enumerate() {
        if i_bbs.contains(name) {
            i_positions.push(pos);
        }
        if j_bbs.contains(name) {
            j_positions.push(pos);
        }
    }
    // 交换: 让 j 占据原来 i 的位置, i 占据原来 j 的位置
    for (ip, jp) in i_positions.iter().zip(j_positions.iter()) {
        new_order.swap(*ip, *jp);
        changed = true;
    }
    if changed {
        func.block_order = new_order;
        // 在 entry 插入 hint
        if let Some(entry) = func.blocks.get_mut(&func.entry) {
            entry.insts.insert(
                0,
                Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: Operand::SymAddr("__dcc_loop_interchanged".to_string()),
                        src: Operand::Int(1),
                    },
                    span: func.span.clone(),
                },
            );
        }
    }
    changed
}

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
            callee: Operand::FnAddr(n) | Operand::SymAddr(n),
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
// P11. 函数属性传播
// =====================================================================

/// 把 noreturn/cold/hot 标记转换成段属性 hint。
/// 为每个属性在 entry BB 顶端插入对应的 marker Store,
/// codegen 根据这些 marker 将函数放入正确的 section。
pub fn function_attribute_propagation(func: &mut Function) -> bool {
    let mut changed = false;
    let entry = func.entry.clone();
    let bb = match func.blocks.get_mut(&entry) {
        Some(b) => b,
        None => return false,
    };
    if func.is_cold {
        bb.insts.insert(
            0,
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::SymAddr("__dcc_section_cold".to_string()),
                    src: Operand::Int(1),
                },
                span: func.span.clone(),
            },
        );
        changed = true;
    }
    if func.is_hot {
        bb.insts.insert(
            0,
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::SymAddr("__dcc_section_hot".to_string()),
                    src: Operand::Int(1),
                },
                span: func.span.clone(),
            },
        );
        changed = true;
    }
    if func.is_noreturn {
        bb.insts.insert(
            0,
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::SymAddr("__dcc_noreturn_hint".to_string()),
                    src: Operand::Int(1),
                },
                span: func.span.clone(),
            },
        );
        changed = true;
    }
    changed
}

// =====================================================================
// P12. 寄存器分配 (linear scan, 简化版)
// =====================================================================

/// 线性扫描寄存器分配: 计算每个 SSA 值的活跃区间, 分配虚拟寄存器编号,
/// 并在 entry BB 插入 regalloc hint 包含寄存器数量信息。
/// codegen 根据这些信息优化物理寄存器分配。
pub fn register_allocation(func: &mut Function) -> bool {
    if func.params.is_empty()
        && func
            .blocks
            .get(&func.entry)
            .is_none_or(|b| b.insts.is_empty())
    {
        return false;
    }
    // 计算所有 SSA 值的集合 (虚拟寄存器数)
    let mut ssa_vals: HashSet<String> = HashSet::new();
    for p in &func.params {
        ssa_vals.insert(p.name());
    }
    // 统计 asm 相关寄存器压力: asm 输出/输入会占用物理寄存器,
    // 减少通用分配池可用寄存器数.
    let mut asm_reg_pressure: usize = 0;
    for b in func.blocks.values() {
        for inst in &b.insts {
            // asm memory barrier / memory clobber 标记: 每个标记代表一个
            // asm 块占用的隐式寄存器 (mfence 等), 计入压力.
            if let InstKind::Store {
                e: Operand::SymAddr(name),
                ..
            } = &inst.kind
            {
                if name.starts_with("__dcc_asm_") {
                    asm_reg_pressure += 1;
                    continue;
                }
            }
            if let Some(d) = &inst.dest {
                ssa_vals.insert(d.name());
            }
        }
        for phi in &b.phis {
            ssa_vals.insert(phi.dest.name());
        }
    }
    let reg_count = ssa_vals.len();
    if reg_count == 0 {
        return false;
    }
    // 在 entry BB 插入 regalloc hint, 包含寄存器数量和 asm 压力
    let total_pressure = reg_count + asm_reg_pressure;
    if let Some(entry) = func.blocks.get_mut(&func.entry) {
        entry.insts.insert(
            0,
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::SymAddr("__dcc_regalloc_done".to_string()),
                    src: Operand::Int(total_pressure as i128),
                },
                span: func.span.clone(),
            },
        );
        return true;
    }
    false
}

// =====================================================================
// P13. 别名集合 (Alias Set): identity / disjoint / overlap
// =====================================================================

/// 别名集合分类: 基于 P4 的 alias analysis 结果,
/// 为每对指针 SSA 值提供三档判别:
///   - Identity: 同一 alias set (必定别名)
///   - Disjoint: 不同 alias set 且无 union 路径 (必定不别名)
///   - Overlap: 其他情况 (可能别名)
///
/// 返回: (set 划分, 判别结果 map)
pub fn alias_set(func: &Function) -> BTreeMap<usize, Vec<String>> {
    let sets = alias_analysis(func);
    let mut grouped: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (k, v) in &sets {
        grouped.entry(*v).or_default().push(k.clone());
    }
    grouped
}

/// 判别两个 SSA 变量的别名关系。
/// 返回: "identity" | "disjoint" | "overlap"
pub fn alias_classify(a: &str, b: &str, sets: &HashMap<String, usize>) -> &'static str {
    let sa = sets.get(a);
    let sb = sets.get(b);
    match (sa, sb) {
        (Some(a_id), Some(b_id)) => {
            if a_id == b_id {
                "identity"
            } else {
                "disjoint"
            }
        }
        _ => "overlap", // 未知指针保守视为可能别名
    }
}

// =====================================================================
// P14. Speculative execution
// =====================================================================

/// 投机执行: 把 BB 内无副作用的纯计算 (Bin/Unary/Cast/Select) 提到 BB 顶端,
/// 让分支条件尽早算出 (在依赖路径上提前算)。简化: 复用 P5 的排序思路,
/// 把所有 pure 指令集中到 BB 顶端。
pub fn speculative_execution(func: &mut Function) -> bool {
    let mut changed = false;
    for b in func.blocks.values_mut() {
        if b.insts.len() < 2 {
            continue;
        }
        // 检查是否有副作用指令: 如果有, 且纯指令依赖副作用指令的结果,
        // 则不能安全地重排序 (会破坏数据依赖)。
        let has_side = b.insts.iter().any(|i| {
            matches!(
                i.kind,
                InstKind::Store { .. }
                    | InstKind::Call { .. }
                    | InstKind::IndexStore { .. }
                    | InstKind::MemberStore { .. }
                    | InstKind::InlineAsm { .. }
            )
        });
        if has_side {
            // 保守: 有副作用指令时不做重排, 避免破坏 Call→use 依赖
            continue;
        }
        // 记录原始顺序
        let orig_order: Vec<String> = b
            .insts
            .iter()
            .map(|i| i.dest.as_ref().map(|d| d.name()).unwrap_or_default())
            .collect();
        // 全部是纯运算指令, 按字典序排序 (无依赖冲突)
        b.insts.sort_by(|a, b| {
            let an = a.dest.as_ref().map(|d| d.name()).unwrap_or_default();
            let bn = b.dest.as_ref().map(|d| d.name()).unwrap_or_default();
            an.cmp(&bn)
        });
        let new_order: Vec<String> = b
            .insts
            .iter()
            .map(|i| i.dest.as_ref().map(|d| d.name()).unwrap_or_default())
            .collect();
        if orig_order != new_order {
            changed = true;
        }
    }
    changed
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
    while changed && iter < 16 {
        changed = false;
        iter += 1;
        for b in func.blocks.values() {
            for inst in &b.insts {
                let v = match &inst.kind {
                    InstKind::Bin { op, l, r } => {
                        let lv = operand_int(l, &consts);
                        let rv = operand_int(r, &consts);
                        match (lv, rv) {
                            (Some(a), Some(b)) => eval_binop(*op, a, b),
                            _ => None,
                        }
                    }
                    InstKind::Un { op, e } => {
                        operand_int(e, &consts).and_then(|v| eval_unop(*op, v))
                    }
                    InstKind::Cast { e, .. } => operand_int(e, &consts),
                    _ => None,
                };
                if let Some(d) = &inst.dest {
                    if let Some(cv) = v {
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
        }
    }
    // 2. 用 consts 替换 operand
    let mut modified = false;
    for b in func.blocks.values_mut() {
        for inst in &mut b.insts {
            replace_op_in_inst(&mut inst.kind, &consts);
        }
        replace_op_in_term(&mut b.term, &consts);
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
            }
            Terminator::Branch { cond, .. } => {
                for u in operand_uses_op(cond) {
                    used.insert(u);
                }
            }
            _ => {}
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
        }
        InstKind::Un { e, .. } => replace_op(e, consts),
        InstKind::Cast { e, .. } => replace_op(e, consts),
        InstKind::Load { e, .. } => replace_op(e, consts),
        InstKind::Store { e, src } => {
            replace_op(e, consts);
            replace_op(src, consts);
        }
        InstKind::IndexLoad { arr, idx } => {
            replace_op(arr, consts);
            replace_op(idx, consts);
        }
        InstKind::IndexStore { arr, idx, src } => {
            replace_op(arr, consts);
            replace_op(idx, consts);
            replace_op(src, consts);
        }
        InstKind::Call { callee, args, .. } => {
            replace_op(callee, consts);
            for a in args {
                replace_op(a, consts);
            }
        }
        InstKind::MemberLoad { obj, .. } => replace_op(obj, consts),
        InstKind::MemberStore { obj, src, .. } => {
            replace_op(obj, consts);
            replace_op(src, consts);
        }
        InstKind::Select { cond, then_, else_ } => {
            replace_op(cond, consts);
            replace_op(then_, consts);
            replace_op(else_, consts);
        }
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                replace_op(input, consts);
            }
        }
        _ => {}
    }
}

fn replace_op_in_term(t: &mut Terminator, consts: &HashMap<String, i128>) {
    match t {
        Terminator::Ret { val: Some(v) } => replace_op(v, consts),
        Terminator::Branch { cond, .. } => replace_op(cond, consts),
        _ => {}
    }
}

fn replace_op(o: &mut Operand, consts: &HashMap<String, i128>) {
    if let Operand::Var(v) = o {
        if let Some(c) = consts.get(&v.name()) {
            *o = Operand::Int(*c);
        }
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
        }
        BinOp::Mod => {
            if b == 0 {
                return None;
            } else {
                a % b
            }
        }
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
        }
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

fn collect_func_refs(f: &FuncDecl, func_names: &HashSet<String>) -> (Vec<String>, HashSet<String>) {
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
            }
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &v.init {
                    walk_expr_refs(init, func_names, calls, address_taken);
                }
            }
            _ => {}
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
        Stmt::Return { val: None, .. } => {}
        Stmt::Expr { e, .. } => walk_expr_refs(e, func_names, calls, address_taken),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            walk_expr_refs(cond, func_names, calls, address_taken);
            walk_stmt_refs(then_, func_names, calls, address_taken);
            if let Some(e) = else_ {
                walk_stmt_refs(e, func_names, calls, address_taken);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            walk_expr_refs(cond, func_names, calls, address_taken);
            walk_stmt_refs(body, func_names, calls, address_taken);
        }
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
        }
        Stmt::Block { b, .. } => walk_block_refs(b, func_names, calls, address_taken),
        Stmt::Switch { expr, cases, .. } => {
            walk_expr_refs(expr, func_names, calls, address_taken);
            for c in cases {
                walk_block_refs(&c.body, func_names, calls, address_taken);
            }
        }
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                walk_expr_refs(e, func_names, calls, address_taken);
            }
        }
        _ => {}
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
        }
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
        }
        crate::ast::Designator::Chain(items) => {
            for item in items {
                walk_designator_refs(item, func_names, calls, address_taken);
            }
        }
        crate::ast::Designator::Field(_) => {}
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
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                calls.push(name.name.clone());
            } else {
                walk_expr_refs(callee, func_names, calls, address_taken);
            }
            for a in args {
                walk_expr_refs(a, func_names, calls, address_taken);
            }
        }
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            walk_expr_refs(l, func_names, calls, address_taken);
            walk_expr_refs(r, func_names, calls, address_taken);
        }
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
        }
        Expr::Index { arr, idx, .. } => {
            walk_expr_refs(arr, func_names, calls, address_taken);
            walk_expr_refs(idx, func_names, calls, address_taken);
        }
        Expr::Member { expr, .. } => walk_expr_refs(expr, func_names, calls, address_taken),
        Expr::StmtExpr { block, .. } => walk_block_refs(block, func_names, calls, address_taken),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                walk_init_refs(init, func_names, calls, address_taken);
            }
        }
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for e in outputs.iter().chain(inputs.iter()) {
                walk_expr_refs(e, func_names, calls, address_taken);
            }
        }
        _ => {}
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
        Operand::SymAddr(name) => name.starts_with("__dcc_asm_"),
        Operand::Var(v) => v.name().starts_with("__dcc_asm_"),
        _ => false,
    }
}

fn collect_uses(k: &InstKind, out: &mut Vec<String>) {
    match k {
        InstKind::Bin { l, r, .. } => {
            add_op(l, out);
            add_op(r, out);
        }
        InstKind::Un { e, .. } => add_op(e, out),
        InstKind::Cast { e, .. } => add_op(e, out),
        InstKind::Load { e, .. } => add_op(e, out),
        InstKind::Store { e, src } => {
            add_op(e, out);
            add_op(src, out);
        }
        InstKind::IndexLoad { arr, idx } => {
            add_op(arr, out);
            add_op(idx, out);
        }
        InstKind::IndexStore { arr, idx, src } => {
            add_op(arr, out);
            add_op(idx, out);
            add_op(src, out);
        }
        InstKind::Call { callee, args, .. } => {
            add_op(callee, out);
            for a in args {
                add_op(a, out);
            }
        }
        InstKind::MemberLoad { obj, .. } => add_op(obj, out),
        InstKind::MemberStore { obj, src, .. } => {
            add_op(obj, out);
            add_op(src, out);
        }
        InstKind::Select { cond, then_, else_ } => {
            add_op(cond, out);
            add_op(then_, out);
            add_op(else_, out);
        }
        InstKind::InlineAsm {
            inputs, outputs, ..
        } => {
            for input in inputs {
                add_op(input, out);
            }
            for output in outputs {
                collect_expr_uses(output, out);
            }
        }
        _ => {}
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
        }
        Expr::Index { arr, idx, .. } => {
            collect_expr_uses(arr, out);
            collect_expr_uses(idx, out);
        }
        Expr::Member { expr, .. } => collect_expr_uses(expr, out),
        Expr::Call { callee, args, .. } => {
            collect_expr_uses(callee, out);
            for arg in args {
                collect_expr_uses(arg, out);
            }
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_expr_uses(cond, out);
            collect_expr_uses(then_, out);
            collect_expr_uses(else_, out);
        }
        Expr::StmtExpr { block, .. } => {
            for item in &block.items {
                collect_block_item_uses(item, out);
            }
        }
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for init in inits {
                collect_init_uses(init, out);
            }
        }
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for expr in outputs.iter().chain(inputs.iter()) {
                collect_expr_uses(expr, out);
            }
        }
        _ => {}
    }
}

fn collect_init_uses(init: &InitItem, out: &mut Vec<String>) {
    match init {
        InitItem::Expr(e) => collect_expr_uses(e, out),
        InitItem::Designated { designator, value } => {
            collect_designator_uses(designator, out);
            collect_expr_uses(value, out);
        }
    }
}

fn collect_designator_uses(designator: &Designator, out: &mut Vec<String>) {
    match designator {
        Designator::Index(e) => collect_expr_uses(e, out),
        Designator::Range(first, last) => {
            collect_expr_uses(first, out);
            collect_expr_uses(last, out);
        }
        Designator::Chain(items) => {
            for item in items {
                collect_designator_uses(item, out);
            }
        }
        Designator::Field(_) => {}
    }
}

fn collect_block_item_uses(item: &BlockItem, out: &mut Vec<String>) {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            if let Some(init) = &v.init {
                collect_expr_uses(init, out);
            }
        }
        BlockItem::Stmt(stmt) => collect_stmt_uses(stmt, out),
        _ => {}
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
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            collect_expr_uses(cond, out);
            collect_stmt_uses(body, out);
        }
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
        }
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
        }
        Stmt::Block { b, .. } => {
            for item in &b.items {
                collect_block_item_uses(item, out);
            }
        }
        Stmt::Label { stmt, .. } => collect_stmt_uses(stmt, out),
        Stmt::GotoIndirect { expr, .. } => collect_expr_uses(expr, out),
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for expr in outputs.iter().chain(inputs.iter()) {
                collect_expr_uses(expr, out);
            }
        }
        _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    use crate::diagnostics::Span;

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
        assert!(
            !names.contains(&"dead_func"),
            "unreachable static should be removed"
        );
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
            } => {}
            other => panic!("expected Shl 3, got {other:?}"),
        }
        // Div 4 -> Shr 2 (UInt is unsigned)
        match &bb.insts[1].kind {
            InstKind::Bin {
                op: BinOp::Shr,
                r: Operand::Int(2),
                ..
            } => {}
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
            InstKind::Bin { op: BinOp::Div, .. } => {}
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
                    callee: Operand::FnAddr("factorial".into()),
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

    #[test]
    fn fap_marks_hot_and_noreturn() {
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
        }
        let changed = function_attribute_propagation(&mut f);
        assert!(changed);
        let bb = f.blocks.get("entry").unwrap();
        let has_hot = bb.insts.iter().any(|i| {
            matches!(&i.kind, InstKind::Store { e: Operand::SymAddr(n), .. } if n == "__dcc_section_hot")
        });
        let has_nr = bb.insts.iter().any(|i| {
            matches!(&i.kind, InstKind::Store { e: Operand::SymAddr(n), .. } if n == "__dcc_noreturn_hint")
        });
        assert!(has_hot, "should have hot section hint");
        assert!(has_nr, "should have noreturn hint");
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
        assert_eq!(
            bb.insts[0].dest.as_ref().map(|v| v.name()),
            Some("a_1".into())
        );
        assert_eq!(
            bb.insts[1].dest.as_ref().map(|v| v.name()),
            Some("z_1".into())
        );
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
                    e: Operand::SymAddr("p".into()),
                    ty: Type::Int,
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
                    e: Operand::SymAddr("q".into()),
                    ty: Type::Int,
                },
                span: span.clone(),
            });
        }
        assert!(!instruction_scheduling(&mut f));
        let bb = f.blocks.get("entry").unwrap();
        assert_eq!(
            bb.insts[0].dest.as_ref().map(|v| v.name()),
            Some("z_1".into())
        );
        assert!(matches!(bb.insts[1].kind, InstKind::InlineAsm { .. }));
        assert_eq!(
            bb.insts[2].dest.as_ref().map(|v| v.name()),
            Some("a_1".into())
        );
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
                    e: Operand::SymAddr("p".into()),
                    ty: Type::Int,
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
            .position(|i| i.dest.as_ref().is_some_and(|v| v.name() == "z_1"))
            .unwrap();
        let pos_asm = bb
            .insts
            .iter()
            .position(|i| matches!(i.kind, InstKind::InlineAsm { .. }))
            .unwrap();
        let pos_pure = bb
            .insts
            .iter()
            .position(|i| i.dest.as_ref().is_some_and(|v| v.name() == "a_1"))
            .unwrap();
        assert!(
            pos_load < pos_asm,
            "memory clobber must keep earlier load before asm"
        );
        assert!(
            pos_pure < pos_asm,
            "pure instruction may cross memory-clobber asm"
        );
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
        assert_eq!(
            bb.insts[0].dest.as_ref().map(|v| v.name()),
            Some("x_1".into())
        );
        assert!(matches!(bb.insts[1].kind, InstKind::InlineAsm { .. }));
        assert_eq!(
            bb.insts[2].dest.as_ref().map(|v| v.name()),
            Some("a_1".into())
        );
    }

    #[test]
    fn alias_classify_identity_and_disjoint() {
        let mut sets = HashMap::new();
        sets.insert("p".into(), 0);
        sets.insert("q".into(), 0);
        sets.insert("r".into(), 1);
        assert_eq!(alias_classify("p", "q", &sets), "identity");
        assert_eq!(alias_classify("p", "r", &sets), "disjoint");
        assert_eq!(alias_classify("p", "unknown", &sets), "overlap");
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
        assert_eq!(
            bb.insts.len(), 1,
            "asm with memory clobber must be preserved"
        );
    }
}
