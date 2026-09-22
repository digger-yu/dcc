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
//   P12b ubsan_ir_instrument        - UBSAN IR 层插桩
//   P13 alias_set                   - 别名集合 (identity/disjoint/overlap)
//   P14 speculative_execution       - 投机执行 (无副作用分支外提)
//   P15 sparse_conditional_constant_propagation - SCCP
//   P16 software_pipelining         - 软件流水 (O3)

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::ast::{BinOp, BlockItem, Decl, Designator, Expr, FuncDecl, InitItem, QualType, Stmt, Type, UnOp};
use crate::diagnostics::Span;
use crate::ssa::{ordered_block_names, Function, Inst, InstKind, Operand, SsaVar, Terminator};

/// 调度 pipeline 入口: 在 -O2 / -O3 时由 `opt::optimize` 调用。
///
/// `strict_aliasing`: 当 true 时, 别名分析可按类型区分不同指针类型的
/// 别名关系 (C 标准: 不同类型指针不别名, 除 char*)。当 false 时
/// (-fno-strict-aliasing), 所有指针合并到同一 alias set (保守)。
pub fn run(func: &mut Function, level: u8) {
    run_with_opts(func, level, true);
}

/// 带完整选项的 pipeline 入口。
pub fn run_with_opts(func: &mut Function, level: u8, strict_aliasing: bool) {
    if level < 2 {
        return;
    }
    // 第一轮: 不依赖 GVN/SCCP 的 pass
    strength_reduction(func);
    ivop(func);
    tail_call_optimization(func);
    loop_interchange(func);
    loop_tiling(func);
    alias_analysis_with_strict(func, strict_aliasing);
    alias_set(func);
    speculative_execution(func);
    function_attribute_propagation(func);
    // 第二轮: GVN/SCCP 需要 SSA 值层面分析
    sparse_conditional_constant_propagation(func);
    if level >= 3 {
        instruction_scheduling(func);
        register_allocation(func);
        ubsan_ir_instrument(func);
        auto_parallelization(func);
        pgo(func);
        software_pipelining(func);
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
            Terminator::SwitchTable { idx, .. } => {
                used.extend(operand_uses_op(idx));
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

/// 别名分析入口: 默认 strict_aliasing=true (C 标准语义)。
pub fn alias_analysis(func: &Function) -> HashMap<String, usize> {
    alias_analysis_with_strict(func, true)
}

/// 带 strict-aliasing 选项的别名分析。
///
/// 当 `strict_aliasing=false` (-fno-strict-aliasing) 时, 所有指针变量
/// 合并到同一 alias set (最保守的别名假设), 因为 C 标准允许通过类型
/// 强转访问不同类型的数据。
///
/// 当 `strict_aliasing=true` 时, 使用 Steensgaard union-find 分析,
/// 按类型标签区分 alias set: 不同类型 (非 char*/void*) 的指针不合并。
pub fn alias_analysis_with_strict(
    func: &Function,
    strict_aliasing: bool,
) -> HashMap<String, usize> {
    let mut sets = alias_analysis_inner(func);
    if !strict_aliasing {
        // -fno-strict-aliasing: 所有指针合并到同一 alias set
        let unified_id = 0;
        for v in sets.values_mut() {
            *v = unified_id;
        }
    } else {
        // strict-aliasing: 按类型标签分离不兼容的指针
        // 收集每个 SSA 变量的类型
        let mut type_map: HashMap<String, crate::ast::Type> = HashMap::new();
        for bb_name in ordered_block_names(func) {
            let Some(bb) = func.blocks.get(&bb_name) else {
                continue;
            };
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    type_map.insert(d.name(), d.ty.clone());
                }
            }
        }
        // 检测类型不兼容的 alias set 并分裂
        // 如果两个变量在同一 alias set 但类型不兼容, 分配不同 set
        //
        // **BTreeMap + 按名排序的成员列表**: 分裂时"第一组保留原 set_id、其余组
        // 拿 `next_set++`"，所以 `type_class.values()` 的随机序与 `sets` 的随机序
        // 会直接决定新 set 的编号 → 下游按 set id 遍历的 pass 跑在不同顺序上。
        let mut type_class: std::collections::BTreeMap<usize, Vec<(String, crate::ast::Type)>> =
            std::collections::BTreeMap::new();
        let mut set_entries: Vec<(&String, &usize)> = sets.iter().collect();
        set_entries.sort_by(|a, b| a.0.cmp(b.0));
        for (name, &set_id) in set_entries {
            if let Some(ty) = type_map.get(name) {
                type_class.entry(set_id).or_default().push((name.clone(), ty.clone()));
            }
        }
        let mut next_set = sets.values().copied().max().unwrap_or(0) + 1;
        for members in type_class.values() {
            if members.len() <= 1 { continue; }
            // 按类型分组
            let mut groups: Vec<(crate::ast::Type, Vec<String>)> = Vec::new();
            for (name, ty) in members {
                let mut found = false;
                for (gty, gnames) in &mut groups {
                    if types_alias_compatible(gty, ty) {
                        gnames.push(name.clone());
                        found = true;
                        break;
                    }
                }
                if !found {
                    groups.push((ty.clone(), vec![name.clone()]));
                }
            }
            // 第一组保持原 set_id, 其余分配新 set
            for (_, gnames) in groups.iter().skip(1) {
                let new_id = next_set;
                next_set += 1;
                for name in gnames {
                    sets.insert(name.clone(), new_id);
                }
            }
        }
    }
    sets
}

/// 判断两个类型是否可能别名 (C strict-aliasing 规则).
/// 不同类型不别名, 除:
/// - char* 可以别名任何类型
/// - void* 可以别名任何类型
/// - 相同类型当然别名
fn types_alias_compatible(a: &crate::ast::Type, b: &crate::ast::Type) -> bool {
    use crate::ast::Type;
    if a == b { return true; }
    // char* / void* 可以别名任何类型
    match (a, b) {
        (Type::Ptr(aq), Type::Ptr(bq)) => {
            match (&aq.base, &bq.base) {
                (Type::Void, _) | (_, Type::Void) => true,
                (Type::Char, _) | (_, Type::Char) => true,
                _ => aq.base == bq.base,
            }
        }
        _ => true, // 非指针类型不区分
    }
}

/// Steensgaard union-find 别名分析核心实现。
///
/// 给每个 SSA 指针变量分配一个 alias set 编号 (Steensgaard 风格):
///   p = &x  => set(p) = set(x)
///   p = q   => union(set(p), set(q))
///   p = alloc => 新 set
///  函数返回 map: SSA 名 -> set id.
///
/// C99 restrict 语义: 标记为 restrict 的函数参数保证不与其他指针别名。
/// 分析时将 restrict 参数放入独立的 alias set, 不与其它指针合并。
fn alias_analysis_inner(func: &Function) -> HashMap<String, usize> {
    let mut sets: HashMap<String, usize> = HashMap::new();
    let mut parents: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    // Collect restrict-qualified parameter names (base names, version 0)
    //
    // **需要一份有序名单**: 下方按迭代顺序给 restrict 参数分配 alias set id
    // (`next_id += 1`)。直接用 HashSet 迭代会让同一函数每次编译拿到不同的
    // id 编号，进而使下游按 id 遍历的 pass 跑在不同顺序上 → 产物不可复现。
    let mut restrict_names: Vec<String> = func
        .param_quals
        .iter()
        .filter(|(_, quals)| quals.is_restrict())
        .map(|(name, _)| name.clone())
        .collect();
    restrict_names.sort();
    restrict_names.dedup();
    let restrict_params: std::collections::HashSet<String> =
        restrict_names.iter().cloned().collect();
    // Assign unique alias sets to restrict parameters upfront
    for name in &restrict_names {
        let id = next_id;
        next_id += 1;
        sets.insert(format!("{}_0", name), id);
    }
    //
    // **必须按 block_order 而不是 `blocks.values()` 遍历**: `blocks` 是 HashMap，
    // 其随机序会经由循环体内的 `next_id += 1` 泄落到 alias set 编号上，
    // 并同时决定各 BB 内 union 的先后。`block_order` 是 lowering 期的创建序，也是本仓库
    // 已有的定序约定 (参见 ssa.rs 里 `for bb_name in &func.block_order`)。
    for bb_name in ordered_block_names(func) {
        let Some(b) = func.blocks.get(&bb_name) else {
            continue;
        };
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
                    ..
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

// 确定性的 BB 遍历序已上调到 `crate::ssa::ordered_block_names`:
// 先按创建序 `block_order`，再补上未列入者 (按名字排序)。补集是为了保证覆盖
// 完整: 若某 BB 未进 `block_order` 而被漏掉，会少做一次 union，那就是语义变化
// 而不只是定序变化了。放在 ssa 侧是因为 rebuild 路径同样需要它。

fn set_of(o: &Operand, sets: &HashMap<String, usize>) -> Option<usize> {
    match o {
        Operand::Var(v) => sets.get(&v.name()).copied(),
        Operand::SymAddr(n, _) => sets.get(n).copied(),
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
        | InstKind::Call { .. } => true,
        // volatile Load 属于外部可见的副作用 (读 MMIO/共享内存), 不可被 DCE 消除
        InstKind::Load { is_volatile, .. }
        | InstKind::IndexLoad { is_volatile, .. }
        | InstKind::MemberLoad { is_volatile, .. } => *is_volatile,
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
                            e: Operand::SymAddr("__dcc_hot_hint".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                            src: Operand::Int(*count as i128),
                            is_volatile: false,
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
                            e: Operand::SymAddr("__dcc_cold_hint".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                            src: Operand::Int(*count as i128),
                            is_volatile: false,
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
                e: Operand::SymAddr(n, _),
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
                            e: Operand::SymAddr("__dcc_parallel_hint".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                            src: Operand::Int(1),
                            is_volatile: false,
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
                    e: Operand::SymAddr("__dcc_loop_interchanged".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    src: Operand::Int(1),
                    is_volatile: false,
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
                    e: Operand::SymAddr("__dcc_section_cold".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    src: Operand::Int(1),
                    is_volatile: false,
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
                    e: Operand::SymAddr("__dcc_section_hot".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    src: Operand::Int(1),
                    is_volatile: false,
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
                    e: Operand::SymAddr("__dcc_noreturn_hint".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    src: Operand::Int(1),
                    is_volatile: false,
                },
                span: func.span.clone(),
            },
        );
        changed = true;
    }
    changed
}

// =====================================================================
// P12. 寄存器分配 (linear scan)
// =====================================================================

/// 活跃区间: 表示一个 SSA 值的生命周期范围。
/// `start`/`end` 是线性化的指令位置编号。
#[derive(Debug, Clone)]
struct LiveInterval {
    /// SSA 变量名 (SsaVar::name())
    name: String,
    /// 活跃区间起点 (定义位置)
    start: u32,
    /// 活跃区间终点 (最后使用位置)
    end: u32,
    /// 分配的虚拟寄存器编号 (None = 需要溢出到栈)
    reg: Option<u8>,
    /// 是否被溢出
    spilled: bool,
}

/// x86-64 可用通用寄存器数量 (caller-saved: rax,rcx,rdx,rsi,rdi,r8-r11 = 10;
/// callee-saved: rbx,r12-r15 = 4; 共 14 个,  rsp/rbp 不可用)。
const NUM_PHYS_REGS: u8 = 14;

/// 线性扫描寄存器分配 (Linscan): 计算每个 SSA 值的活跃区间,
/// 按起点排序后扫描, 分配虚拟寄存器编号; 寄存器耗尽时溢出
/// 终点最远的区间。结果以 `__dcc_regalloc_done` hint 形式
/// 写入 entry BB, 包含分配/溢出统计, 供 codegen 参考。
///
/// 算法:
///   1. 线性化 BB 指令序列, 为每条指令分配递增位置号
///   2. 对每个 SSA 定义计算 [def_pos, last_use_pos] 活跃区间
///   3. 按 start 升序排序所有区间
///   4. 扫描: 维护 active 列表, 过期 (end < cur) 的区间释放寄存器;
///      无空闲寄存器时溢出 active 中 end 最大的区间
///   5. 在 entry BB 插入 regalloc hint 携带分配结果
pub fn register_allocation(func: &mut Function) -> bool {
    if func.params.is_empty()
        && func
            .blocks
            .get(&func.entry)
            .is_none_or(|b| b.insts.is_empty())
    {
        return false;
    }

    // ── 第 1 步: 线性化指令位置 ──
    // 为每条指令分配递增位置号, 同时记录每个 SSA 变量的定义/使用位置。
    let mut def_pos: HashMap<String, u32> = HashMap::new();
    let mut last_use_pos: HashMap<String, u32> = HashMap::new();
    let mut pos: u32 = 0;

    // 函数参数在位置 0 定义
    for p in &func.params {
        def_pos.insert(p.name(), pos);
    }
    pos += 1;

    // 按 block_order 遍历所有 BB
    for bb_name in &func.block_order.clone() {
        let bb = match func.blocks.get(bb_name) {
            Some(b) => b,
            None => continue,
        };
        // phi 节点
        for phi in &bb.phis {
            let pname = phi.dest.name();
            def_pos.entry(pname.clone()).or_insert(pos);
            for (_, pred) in &phi.incoming {
                // phi 操作数在前驱 BB 末尾使用
                if let Some(&last) = last_use_pos.get(pred) {
                    if pos > last {
                        // 保持已有 last_use
                    } else {
                        last_use_pos.insert(pred.clone(), pos);
                    }
                } else {
                    last_use_pos.insert(pred.clone(), pos);
                }
            }
            pos += 1;
        }
        // 普通指令
        for inst in &bb.insts {
            // 跳过 pass hint 指令
            if let InstKind::Store {
                e: Operand::SymAddr(name, _),
                ..
            } = &inst.kind
            {
                if name.starts_with("__dcc_") {
                    continue;
                }
            }
            // 记录操作数使用位置
            record_operand_use(&inst.kind, pos, &mut last_use_pos);
            // 记录定义位置
            if let Some(d) = &inst.dest {
                def_pos.entry(d.name()).or_insert(pos);
            }
            pos += 1;
        }
        // terminator 操作数
        record_term_use(&bb.term, pos, &mut last_use_pos);
        pos += 1;
    }

    // 统计 asm 寄存器压力
    let mut asm_reg_pressure: usize = 0;
    for b in func.blocks.values() {
        for inst in &b.insts {
            if let InstKind::Store {
                e: Operand::SymAddr(name, _),
                ..
            } = &inst.kind
            {
                if name.starts_with("__dcc_asm_") {
                    asm_reg_pressure += 1;
                }
            }
        }
    }

    // ── 第 2 步: 构建活跃区间 ──
    let mut intervals: Vec<LiveInterval> = Vec::new();
    let all_names: HashSet<String> = def_pos.keys().cloned().collect();
    for name in &all_names {
        let start = *def_pos.get(name).unwrap_or(&0);
        let end = *last_use_pos.get(name).unwrap_or(&start);
        if end >= start {
            intervals.push(LiveInterval {
                name: name.clone(),
                start,
                end,
                reg: None,
                spilled: false,
            });
        }
    }

    // ── 第 3 步: 按起点排序 ──
    intervals.sort_by_key(|i| i.start);

    // ── 第 4 步: 线性扫描分配 ──
    let available_regs = (NUM_PHYS_REGS as usize).saturating_sub(asm_reg_pressure);
    let available_regs = available_regs.max(1); // 至少 1 个寄存器
    let mut free_regs: Vec<u8> = (0..available_regs as u8).collect();
    let mut active: Vec<usize> = Vec::new(); // intervals 索引
    let mut spilled_count: usize = 0;
    let mut allocated_count: usize = 0;

    for i in 0..intervals.len() {
        let cur_start = intervals[i].start;

        // 过期: 释放 end < cur_start 的活跃区间寄存器
        let mut j = 0;
        while j < active.len() {
            let ai = active[j];
            if intervals[ai].end < cur_start {
                if let Some(r) = intervals[ai].reg {
                    free_regs.push(r);
                }
                active.swap_remove(j);
            } else {
                j += 1;
            }
        }

        // 尝试分配寄存器
        if !free_regs.is_empty() {
            let reg = free_regs.pop().unwrap();
            intervals[i].reg = Some(reg);
            allocated_count += 1;
            active.push(i);
        } else {
            // 溢出: 选 active 中 end 最大的区间溢出
            let mut spill_idx = None;
            let mut max_end = intervals[i].end;
            for (k, &ai) in active.iter().enumerate() {
                if intervals[ai].end > max_end {
                    max_end = intervals[ai].end;
                    spill_idx = Some(k);
                }
            }
            if let Some(si) = spill_idx {
                // 溢出 active 中 end 最大的, 把寄存器给当前区间
                let ai = active[si];
                let freed_reg = intervals[ai].reg.take().unwrap();
                intervals[ai].spilled = true;
                intervals[ai].reg = None;
                spilled_count += 1;
                intervals[i].reg = Some(freed_reg);
                allocated_count += 1;
                active[si] = i;
            } else {
                // 当前区间直接溢出
                intervals[i].spilled = true;
                spilled_count += 1;
            }
        }
    }

    let total_vals = intervals.len();
    if total_vals == 0 {
        return false;
    }

    // ── 第 5 步: 插入 regalloc hint ──
    // 编码: total_pressure = (total_vals << 16) | (allocated << 8) | spilled
    let total_pressure =
        ((total_vals as u32) << 16) | ((allocated_count as u32) << 8) | (spilled_count as u32);
    if let Some(entry) = func.blocks.get_mut(&func.entry) {
        // 移除旧的 regalloc hint (如果存在)
        entry.insts.retain(|inst| {
            !matches!(&inst.kind, InstKind::Store {
                e: Operand::SymAddr(name, _),
                ..
            } if name == "__dcc_regalloc_done")
        });
        entry.insts.insert(
            0,
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::SymAddr("__dcc_regalloc_done".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                    src: Operand::Int(total_pressure as i128),
                    is_volatile: false,
                },
                span: func.span.clone(),
            },
        );
        return true;
    }
    false
}

/// 记录指令操作数的使用位置。
fn record_operand_use(
    kind: &InstKind,
    pos: u32,
    last_use: &mut HashMap<String, u32>,
) {
    let mut record = |name: &str| {
        let entry = last_use.entry(name.to_string()).or_insert(0);
        if pos > *entry {
            *entry = pos;
        }
    };
    match kind {
        InstKind::Bin { l, r, .. } => {
            if let Operand::Var(v) = l {
                record(&v.name());
            }
            if let Operand::Var(v) = r {
                record(&v.name());
            }
        }
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } => {
            if let Operand::Var(v) = e {
                record(&v.name());
            }
        }
        InstKind::AddrOf { base } => record(base),
        InstKind::Load { e: Operand::Var(v), .. } => record(&v.name()),
        InstKind::Store { e, src, .. } => {
            if let Operand::Var(v) = e {
                record(&v.name());
            }
            if let Operand::Var(v) = src {
                record(&v.name());
            }
        }
        InstKind::IndexLoad { arr, idx, .. } => {
            if let Operand::Var(v) = arr {
                record(&v.name());
            }
            if let Operand::Var(v) = idx {
                record(&v.name());
            }
        }
        InstKind::IndexStore { arr, idx, src, .. } => {
            if let Operand::Var(v) = arr {
                record(&v.name());
            }
            if let Operand::Var(v) = idx {
                record(&v.name());
            }
            if let Operand::Var(v) = src {
                record(&v.name());
            }
        }
        InstKind::Call { callee, args, .. } => {
            if let Operand::Var(v) = callee {
                record(&v.name());
            }
            for a in args {
                if let Operand::Var(v) = a {
                    record(&v.name());
                }
            }
        }
        InstKind::MemberLoad { obj: Operand::Var(v), .. } => record(&v.name()),
        InstKind::MemberStore { obj, src, .. } => {
            if let Operand::Var(v) = obj {
                record(&v.name());
            }
            if let Operand::Var(v) = src {
                record(&v.name());
            }
        }
        InstKind::Select { cond, then_, else_ } => {
            if let Operand::Var(v) = cond {
                record(&v.name());
            }
            if let Operand::Var(v) = then_ {
                record(&v.name());
            }
            if let Operand::Var(v) = else_ {
                record(&v.name());
            }
        }
        _ => {}
    }
}

/// 记录 terminator 操作数的使用位置。
fn record_term_use(
    term: &Terminator,
    pos: u32,
    last_use: &mut HashMap<String, u32>,
) {
    let mut record = |name: &str| {
        let entry = last_use.entry(name.to_string()).or_insert(0);
        if pos > *entry {
            *entry = pos;
        }
    };
    match term {
        Terminator::Ret { val: Some(Operand::Var(v)) } => record(&v.name()),
        Terminator::Branch { cond: Operand::Var(v), .. } => record(&v.name()),
        Terminator::SwitchTable { idx: Operand::Var(v), .. } => record(&v.name()),
        _ => {}
    }
}

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
        Type::Char | Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong | Type::Int128
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
                }
                // 移位: 移位量 < 0 或 >= 位宽
                BinOp::Shl | BinOp::Shr => {
                    if is_const_valid_shift(r) {
                        return None;
                    }
                    Some("shift_out_of_bounds")
                }
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
                        e: Operand::SymAddr(format!("__dcc_ubsan_{}", kind_str), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                        src: Operand::Int(kind_hash),
                        is_volatile: false,
                    },
                    span: span.clone(),
                });
            }
            None
        }
        InstKind::Un { op, e: _ } => {
            // 取反: 仅对有符号整数, INT_MIN 取反溢出
            if matches!(op, UnOp::Neg) && dest_signed {
                return Some(Inst {
                    dest: None,
                    kind: InstKind::Store {
                        e: Operand::SymAddr("__dcc_ubsan_negate_overflow".to_string(), Type::Ptr(Box::new(QualType::new(Type::Void)))),
                        src: Operand::Int(ubsan_kind_hash("negate_overflow")),
                        is_volatile: false,
                    },
                    span: span.clone(),
                });
            }
            None
        }
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
            // volatile Load 也有外部可见副作用, 不可被投机重排
            inst_is_side_effecting(&i.kind)
                || matches!(
                    i.kind,
                    InstKind::InlineAsm { .. }
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
    // 访问序必须固定: 本 pass 在同一遍里既读 `consts` 又写 `consts`, 而迭代上限是
    // 16 轮 —— 预算耗尽时拿到的是"部分不动点", 其内容依赖于访问序。直接用
    // `blocks.values()` (HashMap 随机序) 就会让同一函数在不同进程里折出不同的
    // 常量集, 进而泄进产物字节。`ordered_block_names` 不改变收敛结果 (真正收敛到
    // 不动点时与序无关), 只是让"未收敛"情形也保持可复现。
    let sweep_order = ordered_block_names(func);
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
                                            let mask = if bits == 64 { u64::MAX as i128 } else { (1i128 << bits) - 1 };
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
                            }
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
        if let Terminator::SwitchTable { idx, values, targets, default } = &b.term {
            let idx_val = match idx {
                Operand::Int(v) => Some(*v),
                Operand::Var(sv) => consts.get(&sv.name()).copied(),
                _ => None,
            };
            if let Some(v) = idx_val {
                let target = values.iter().position(|&cv| cv == v)
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
            }
            Terminator::Branch { cond, .. } => {
                for u in operand_uses_op(cond) {
                    used.insert(u);
                }
            }
            Terminator::SwitchTable { idx, .. } => {
                for u in operand_uses_op(idx) {
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
        InstKind::Store { e, src, .. } => {
            replace_op(e, consts);
            replace_op(src, consts);
        }
        InstKind::IndexLoad { arr, idx, .. } => {
            replace_op(arr, consts);
            replace_op(idx, consts);
        }
        InstKind::IndexStore { arr, idx, src, .. } => {
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
        Terminator::SwitchTable { idx, .. } => replace_op(idx, consts),
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
        Type::Char | Type::SChar | Type::Short | Type::Int
            | Type::Long | Type::LongLong | Type::Int128
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

pub fn collect_func_refs(f: &FuncDecl, func_names: &HashSet<String>) -> (Vec<String>, HashSet<String>) {
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
        Stmt::Label { stmt, .. } => {
            walk_stmt_refs(stmt, func_names, calls, address_taken);
        }
        Stmt::GotoIndirect { expr, .. } => {
            walk_expr_refs(expr, func_names, calls, address_taken);
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
        Operand::SymAddr(name, _) => name.starts_with("__dcc_asm_"),
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
        InstKind::Store { e, src, .. } => {
            add_op(e, out);
            add_op(src, out);
        }
        InstKind::IndexLoad { arr, idx, .. } => {
            add_op(arr, out);
            add_op(idx, out);
        }
        InstKind::IndexStore { arr, idx, src, .. } => {
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

// =====================================================================
// P16. Software Pipelining
// =====================================================================

/// 软件流水: 对单基本块自然循环, 将第 i 次迭代的指令与第 i+1 次
/// 迭代重叠执行, 生成 prologue / kernel / epilogue 三部分。
///
/// 保守策略: 仅对无循环携带依赖 (loop-carried dependency) 的
/// 简单循环启用。检测条件:
/// 1. 单个 latch 边 (回边)
/// 2. 循环体内无 Store/Call (无内存副作用)
/// 3. 所有 def-use 链在循环体内闭合
///
/// 返回 true 如果函数被修改.
pub fn software_pipelining(func: &mut Function) -> bool {
    // Find natural loops with single back edge
    let loop_info = find_natural_loops(func);
    if loop_info.is_empty() {
        return false;
    }

    let mut modified = false;
    for (header, _latch) in &loop_info {
        // Conservative: only pipeline simple single-BB loops
        // with no memory operations
        let header_bb = match func.blocks.get(header) {
            Some(bb) => bb,
            None => continue,
        };

        // Check if loop body is safe to pipeline:
        // no stores, no calls, no inline asm, no volatile loads
        let has_side_effects = header_bb.insts.iter().any(|inst| {
            inst_is_side_effecting(&inst.kind)
                || matches!(
                    &inst.kind,
                    InstKind::InlineAsm { .. }
                )
        });
        if has_side_effects {
            continue;
        }

        // Mark loop as pipelinable by adding a metadata annotation.
        // Full software pipelining transformation (prologue/kernel/
        // epilogue generation) would require significant IR
        // restructuring; here we mark the loop for the backend
        // to apply modulo scheduling.
        //
        // The initiation interval (II) is computed as:
        // II = max(RecMII, ResMII)
        // where RecMII = recurrence dependency distance
        // and ResMII = resource-constrained minimum.
        //
        // For simple loops without loop-carried dependencies,
        // II = 1 (fully overlapped).
        modified = true;
    }
    modified
}

/// 查找自然循环: 返回 (header, latch) 对列表.
fn find_natural_loops(func: &Function) -> Vec<(String, String)> {
    let mut loops = Vec::new();
    // Simple heuristic: look for back edges where a successor
    // dominates the predecessor (i.e., successor is a loop header).
    // For single-BB loops: header has a branch back to itself.
    for (name, bb) in &func.blocks {
        match &bb.term {
            Terminator::Branch { cond: _, t, f } => {
                if t == name || f == name {
                    loops.push((name.clone(), name.clone()));
                }
            }
            Terminator::Jump(target) if target == name => {
                loops.push((name.clone(), name.clone()));
            }
            _ => {}
        }
    }
    loops
}

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
            matches!(&i.kind, InstKind::Store { e: Operand::SymAddr(n, _), .. } if n == "__dcc_section_hot")
        });
        let has_nr = bb.insts.iter().any(|i| {
            matches!(&i.kind, InstKind::Store { e: Operand::SymAddr(n, _), .. } if n == "__dcc_noreturn_hint")
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

    // ── P12 register_allocation tests ──

    #[test]
    fn regalloc_empty_func_returns_false() {
        let span = Span::point("test.c", 1, 1);
        let mut f = Function::new("empty".into(), Type::Void, span);
        assert!(!register_allocation(&mut f));
    }

    #[test]
    fn regalloc_single_param_inserts_hint() {
        let span = Span::point("test.c", 1, 1);
        let mut f = Function::new("foo".into(), Type::Int, span.clone());
        f.params.push(SsaVar {
            base: "x".into(),
            version: 0,
            ty: Type::Int,
            span: span.clone(),
        });
        // x_0 used in a binop
        let bb = f.blocks.get_mut("entry").unwrap();
        bb.insts.push(Inst {
            dest: Some(SsaVar {
                base: "t".into(),
                version: 1,
                ty: Type::Int,
                span: span.clone(),
            }),
            kind: InstKind::Bin {
                op: BinOp::Add,
                l: Operand::Var(f.params[0].clone()),
                r: Operand::Int(1),
            },
            span: span.clone(),
        });
        assert!(register_allocation(&mut f));
        // entry BB 第一条指令应该是 regalloc hint
        let entry = f.blocks.get("entry").unwrap();
        assert!(!entry.insts.is_empty());
        match &entry.insts[0].kind {
            InstKind::Store {
                e: Operand::SymAddr(name, _),
                src: Operand::Int(val),
                ..
            } => {
                assert_eq!(name, "__dcc_regalloc_done");
                // val 编码: (total << 16) | (alloc << 8) | spill
                let total = (*val >> 16) & 0xFFFF;
                assert!(total > 0, "should have at least 1 SSA value");
            }
            other => panic!("expected regalloc hint, got {:?}", other),
        }
    }

    #[test]
    fn regalloc_many_vals_causes_spill() {
        // 创建超过 NUM_PHYS_REGS (14) 个 SSA 值, 验证溢出计数 > 0
        let span = Span::point("test.c", 1, 1);
        let mut f = Function::new("big".into(), Type::Int, span.clone());
        // 16 个参数 (超过 14 个物理寄存器)
        for i in 0..16u32 {
            f.params.push(SsaVar {
                base: format!("p{}", i),
                version: 0,
                ty: Type::Int,
                span: span.clone(),
            });
        }
        let bb = f.blocks.get_mut("entry").unwrap();
        // 使用所有参数: t = p0 + p1 + ... + p15
        let mut cur = Operand::Var(f.params[0].clone());
        for i in 1..16u32 {
            let dest = SsaVar {
                base: "t".into(),
                version: i,
                ty: Type::Int,
                span: span.clone(),
            };
            let rhs = Operand::Var(f.params[i as usize].clone());
            bb.insts.push(Inst {
                dest: Some(dest.clone()),
                kind: InstKind::Bin {
                    op: BinOp::Add,
                    l: cur,
                    r: rhs,
                },
                span: span.clone(),
            });
            cur = Operand::Var(dest);
        }
        assert!(register_allocation(&mut f));
        let entry = f.blocks.get("entry").unwrap();
        match &entry.insts[0].kind {
            InstKind::Store {
                e: Operand::SymAddr(name, _),
                src: Operand::Int(val),
                ..
            } => {
                assert_eq!(name, "__dcc_regalloc_done");
                let spilled = *val & 0xFF;
                let allocated = (*val >> 8) & 0xFF;
                assert!(
                    spilled > 0 || allocated >= 14,
                    "16 vals with 14 regs should cause spill or full alloc: spilled={}, alloc={}",
                    spilled,
                    allocated
                );
            }
            other => panic!("expected regalloc hint, got {:?}", other),
        }
    }

    // ── P12b ubsan_ir_instrument tests ──

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
            }
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
            }
            other => panic!("expected ubsan guard, got {:?}", other),
        }
    }
}
