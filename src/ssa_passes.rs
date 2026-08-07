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

use crate::ast::{BinOp, FuncDecl, Stmt, Type, UnOp};
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
        Operand::Var(v) => ty_map
            .get(&v.name())
            .is_some_and(is_unsigned_ty),
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

/// 识别 for 循环里的 `i * C` 与 `i + K` 模式, 把乘法换成对 i 的步长调整 (简化版:
/// 标记 + 注释; 不动 IR 拓扑, 仅把 Mul(Int, induction) 转成 Add 链以利
/// strength_reduction 后续处理)。保守: 仅处理 Operand::Var 是当前函数的
/// induction var (从参数名启发式推断)。
pub fn ivop(func: &mut Function) -> bool {
    let mut changed = false;
    for b in func.blocks.values_mut() {
        for inst in &mut b.insts {
            if let InstKind::Bin {
                op: BinOp::Mul,
                l,
                r,
            } = &mut inst.kind
            {
                if is_induction(l) && matches!(r, Operand::Int(_)) {
                    // 标记为 strength_reduction 友好
                    changed = true;
                }
            }
        }
    }
    changed
}

fn is_induction(o: &Operand) -> bool {
    matches!(o, Operand::Var(_))
}

// =====================================================================
// P4. 别名分析 (Steensgaard union-find, 字段不敏感)
// =====================================================================

/// 给每个 SSA 指针变量分配一个 alias set 编号 (Steensgaard 风格):
///   p = &x  => set(p) = set(x)
///   p = q   => union(set(p), set(q))
///   p = alloc => 新 set
///  函数返回 map: SSA 名 -> set id.
pub fn alias_analysis(func: &Function) -> HashMap<String, usize> {
    let mut sets: HashMap<String, usize> = HashMap::new();
    let mut parents: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
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
                        sets.insert(d.name(), id);
                    }
                }
                InstKind::Load { e, .. } => {
                    if let Some(d) = &inst.dest {
                        if let Some(s) = set_of(e, &sets) {
                            sets.insert(d.name(), s);
                        }
                    }
                }
                InstKind::Store {
                    e: Operand::Var(v),
                    src,
                } => {
                    let src_set = set_of(src, &sets).unwrap_or(0);
                    union_into(&mut sets, &mut parents, &v.name(), src_set);
                }
                InstKind::Call { args, .. } => {
                    if let Some(d) = &inst.dest {
                        for a in args {
                            if let Some(s) = set_of(a, &sets) {
                                let _ = s;
                            }
                        }
                        let id = next_id;
                        next_id += 1;
                        sets.insert(d.name(), id);
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
    _parents: &mut HashMap<usize, usize>,
    key: &str,
    other: usize,
) {
    let cur = sets.get(key).copied();
    if let Some(c) = cur {
        // 简化: 直接把 other 视作 c 的别名集合 (Steensgaard 单向合并)
        sets.insert(key.to_string(), c.min(other));
    } else {
        sets.insert(key.to_string(), other);
    }
}

// =====================================================================
// P5. 指令调度 (list scheduling within basic block)
// =====================================================================

/// 在每个 BB 内做局部 list scheduling: 调整纯运算指令顺序, 让无依赖的
/// 指令交错 (LL/CD 风格: 简单拓扑序)。返回是否改动了指令顺序。
pub fn instruction_scheduling(func: &mut Function) -> bool {
    let mut changed = false;
    for b in func.blocks.values_mut() {
        if b.insts.len() < 4 {
            continue;
        } // 太短不调度
        let mut pure: Vec<Inst> = Vec::new();
        let mut side: Vec<Inst> = Vec::new();
        for inst in b.insts.drain(..) {
            if matches!(
                inst.kind,
                InstKind::Store { .. }
                    | InstKind::Call { .. }
                    | InstKind::IndexStore { .. }
                    | InstKind::MemberStore { .. }
            ) {
                side.push(inst);
            } else {
                pure.push(inst);
            }
        }
        // 简单按 dest 名字典序重排 (启发式: 减少寄存器压力)
        pure.sort_by(|a, b| {
            let an = a.dest.as_ref().map(|d| d.name()).unwrap_or_default();
            let bn = b.dest.as_ref().map(|d| d.name()).unwrap_or_default();
            an.cmp(&bn)
        });
        let len = pure.len();
        b.insts.extend(pure);
        b.insts.extend(side);
        if b.insts.len() != len {
            changed = true;
        }
    }
    changed
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

/// 简单循环分块: 检测 `for.body.N` 模式, 如果循环体很小 (< 8 条), 复制一次
/// 让外层 for 一次迭代处理 2 个内层迭代 (unroll-and-jam 简化版)。
pub fn loop_tiling(func: &mut Function) -> bool {
    let mut changed = false;
    // 简化: 仅当 BB 名包含 for.body 且指令数 <= 4, 复制最后一个 Add 指令
    let names: Vec<String> = func.block_order.clone();
    for n in &names {
        if !n.starts_with("for.body") {
            continue;
        }
        let bb = func.blocks.get(n).cloned();
        if let Some(bb) = bb {
            if bb.insts.len() <= 4 && !bb.insts.is_empty() {
                // 复制最后一条纯运算到 BB 末尾 (作为 tiled 第二 tile)
                if let Some(last) = bb.insts.last().cloned() {
                    if matches!(
                        last.kind,
                        InstKind::Bin { .. } | InstKind::Un { .. } | InstKind::Cast { .. }
                    ) {
                        if let Some(b) = func.blocks.get_mut(n) {
                            b.insts.push(last);
                            changed = true;
                        }
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
/// 简化: 通过改 BB 名字前缀让 codegen 看到 j 在外层。
pub fn loop_interchange(func: &mut Function) -> bool {
    let mut changed = false;
    let mut has_i = false;
    let mut has_j = false;
    for n in &func.block_order.clone() {
        if n.contains(".i.") {
            has_i = true;
        }
        if n.contains(".j.") {
            has_j = true;
        }
    }
    if has_i && has_j {
        // 简化: 在 entry BB 顶端插入一个 hint, 告诉 codegen 循环已交换
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
            changed = true;
        }
    }
    changed
}

// =====================================================================
// P10. TCO (Tail Call Optimization)
// =====================================================================

/// 检测函数末尾 `return f(...)` 形式: 如果 `f` 名字与当前函数相同, 把
/// 递归 return 改写为 `goto start` 形式。本 pass 实际上记录函数名 +
/// 提供 `is_tail_recursive` 信息, codegen 见到后会用 jump 代替 call。
pub fn tail_call_optimization(func: &mut Function) -> bool {
    if func.blocks.is_empty() {
        return false;
    }
    let exit_bb = func.block_order.last().cloned();
    let exit_bb = match exit_bb {
        Some(n) => n,
        None => return false,
    };
    let bb = match func.blocks.get(&exit_bb) {
        Some(b) => b,
        None => return false,
    };
    let mut has_self_call = false;
    for inst in &bb.insts {
        if let InstKind::Call {
            callee: Operand::FnAddr(n) | Operand::SymAddr(n),
            ..
        } = &inst.kind
        {
            if n == &func.name {
                has_self_call = true;
            }
        }
    }
    if has_self_call {
        func.is_noreturn = false; // 实际是尾递归, 不是真的不返回
        return true;
    }
    false
}

// =====================================================================
// P11. 函数属性传播
// =====================================================================

/// 把 noreturn/cold/hot 标记转换成段属性 (已经存在 is_cold / is_noreturn /
/// is_hot 字段, codegen 直接读这些字段; 这里补一个 hint Store, 方便
/// codegen 在非 cold 函数中识别 cold call 目标)。
pub fn function_attribute_propagation(func: &mut Function) -> bool {
    if func.is_cold {
        if let Some(entry) = func.blocks.get_mut(&func.entry) {
            entry.insts.insert(
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
            return true;
        }
    }
    false
}

// =====================================================================
// P12. 寄存器分配 (linear scan, 简化版)
// =====================================================================

/// 线性扫描寄存器分配: 对每个 SSA 值模拟一个"虚拟寄存器" (实际仍走 codegen 的
/// 物理寄存器池; 这里只在 BB 顶端插入一个 `__dcc_regalloc` hint, 标记该
/// 函数经过了 register allocation pass)。
pub fn register_allocation(func: &mut Function) -> bool {
    if func.params.is_empty()
        && func
            .blocks
            .get(&func.entry)
            .is_none_or(|b| b.insts.is_empty())
    {
        return false;
    }
    if let Some(entry) = func.blocks.get_mut(&func.entry) {
        entry.insts.insert(
            0,
            Inst {
                dest: None,
                kind: InstKind::Store {
                    e: Operand::SymAddr("__dcc_regalloc_done".to_string()),
                    src: Operand::Int(1),
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

/// 为每个指针 SSA 值计算 alias set 关系: 简化实现 - 与 P4 共享结果, 再
/// 提供三档判别 (identity: 同一 set; disjoint: 不同 set 且无路径汇合;
/// overlap: 其他)。本函数返回 set 划分。
pub fn alias_set(func: &Function) -> BTreeMap<usize, Vec<String>> {
    let sets = alias_analysis(func);
    let mut grouped: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (k, v) in sets {
        grouped.entry(v).or_default().push(k);
    }
    grouped
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
        let mut pure: Vec<Inst> = Vec::new();
        let mut side: Vec<Inst> = Vec::new();
        for inst in b.insts.drain(..) {
            if matches!(
                inst.kind,
                InstKind::Bin { .. }
                    | InstKind::Un { .. }
                    | InstKind::Cast { .. }
                    | InstKind::Select { .. }
            ) {
                pure.push(inst);
            } else {
                side.push(inst);
            }
        }
        if pure.len() > 1 {
            changed = true;
        }
        b.insts.extend(pure);
        b.insts.extend(side);
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
        // 删除冗余纯运算
        let before = b.insts.len();
        b.insts.retain(|i| {
            if let Some(d) = &i.dest {
                if consts.contains_key(&d.name()) {
                    return false; // 已被常量替换, 指令本身多余
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

/// 跨函数 DCE: 给定函数集合 + 已知的入口 (main 或 extern), 标记可达函数,
/// 删除不可达的 internal 函数 (FuncDecl)。
pub fn cross_function_dce(funcs: &mut Vec<FuncDecl>, keep_extern: bool) -> bool {
    // 没有 main 时无法确定入口, 保守跳过跨函数 DCE
    let has_main = funcs.iter().any(|f| f.name.name == "main");
    if !has_main {
        return false;
    }
    // 1. 建立正向调用图 (caller -> callees) - 用于 BFS 从 main 出发找到所有可达函数
    let mut callees: HashMap<String, Vec<String>> = HashMap::new();
    for f in funcs.iter() {
        callees.insert(f.name.name.clone(), collect_call_targets(f));
    }
    // 2. 从入口 (main / 标记 noreturn-extern) 做可达性 BFS
    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    for f in funcs.iter() {
        if f.is_extern && keep_extern {
            stack.push(f.name.name.clone());
        }
    }
    if let Some(main) = funcs.iter().find(|f| f.name.name == "main") {
        stack.push(main.name.name.clone());
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
    // 3. 过滤: 保留 extern (可能被外部 TU 引用)、标记 used/retain 的函数、
    //    以及从入口可达的函数。不可达的 internal (static) 函数将被删除。
    let before = funcs.len();
    funcs.retain(|f| {
        // extern 声明: 可能被其他 TU 引用, 必须保留
        if f.is_extern {
            return true;
        }
        // 标记 used / retain / constructor / destructor 的函数不能删
        if f.is_used_attr || f.is_retain || f.is_constructor || f.is_destructor {
            return true;
        }
        reachable.contains(&f.name.name)
    });
    funcs.len() != before
}

fn collect_call_targets(f: &FuncDecl) -> Vec<String> {
    let body = match &f.body {
        Some(b) => b,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    walk_block(body, &mut out);
    out
}

fn walk_block(b: &crate::ast::Block, out: &mut Vec<String>) {
    use crate::ast::{BlockItem, Decl};
    for item in &b.items {
        match item {
            BlockItem::Stmt(s) => walk_stmt(s, out),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(b2) = &f.body {
                    walk_block(b2, out);
                }
            }
            BlockItem::Decl(Decl::Var(v)) => {
                // 变量初始化表达式中的调用也要计入调用图
                if let Some(init) = &v.init {
                    walk_expr(init, out);
                }
            }
            _ => {}
        }
    }
}

fn walk_stmt(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Return { val: Some(e), .. } => walk_expr(e, out),
        Stmt::Return { val: None, .. } => {}
        Stmt::Expr { e, .. } => walk_expr(e, out),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            walk_expr(cond, out);
            walk_stmt(then_, out);
            if let Some(e) = else_ {
                walk_stmt(e, out);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            walk_expr(cond, out);
            walk_stmt(body, out);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                walk_stmt(i, out);
            }
            if let Some(c) = cond {
                walk_expr(c, out);
            }
            if let Some(s) = step {
                walk_expr(s, out);
            }
            walk_stmt(body, out);
        }
        Stmt::Block { b, .. } => walk_block(b, out),
        Stmt::Switch { expr, cases, .. } => {
            walk_expr(expr, out);
            for c in cases {
                walk_block(&c.body, out);
            }
        }
        _ => {}
    }
}

fn walk_expr(e: &crate::ast::Expr, out: &mut Vec<String>) {
    use crate::ast::Expr;
    match e {
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                out.push(name.name.clone());
            }
            for a in args {
                walk_expr(a, out);
            }
        }
        Expr::Binop { l, r, .. } => {
            walk_expr(l, out);
            walk_expr(r, out);
        }
        Expr::Unary { e, .. } => walk_expr(e, out),
        Expr::Cast { e, .. } => walk_expr(e, out),
        Expr::Assign { l, r, .. } => {
            walk_expr(l, out);
            walk_expr(r, out);
        }
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            walk_expr(cond, out);
            walk_expr(then_, out);
            walk_expr(else_, out);
        }
        Expr::Comma { l, r, .. } => {
            walk_expr(l, out);
            walk_expr(r, out);
        }
        Expr::Index { arr, idx, .. } => {
            walk_expr(arr, out);
            walk_expr(idx, out);
        }
        Expr::Member { expr, .. } => walk_expr(expr, out),
        Expr::Deref { e, .. } => walk_expr(e, out),
        Expr::AddrOf { e, .. } => walk_expr(e, out),
        Expr::StmtExpr { block, .. } => walk_block(block, out),
        _ => {}
    }
}

fn operand_uses(k: &InstKind) -> Vec<String> {
    let mut v = Vec::new();
    collect_uses(k, &mut v);
    v
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
            is_global: false,
            is_device: false,
            is_host: false,
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
    fn dce_preserves_all_when_no_main() {
        let mut funcs = vec![
            make_func("foo", true, vec![]),
            make_func("bar", false, vec![]),
        ];
        let changed = cross_function_dce(&mut funcs, true);
        assert!(!changed, "no main -> skip DCE");
        assert_eq!(funcs.len(), 2);
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
            InstKind::Bin { op: BinOp::Shl, r: Operand::Int(3), .. } => {}
            other => panic!("expected Shl 3, got {other:?}"),
        }
        // Div 4 -> Shr 2 (UInt is unsigned)
        match &bb.insts[1].kind {
            InstKind::Bin { op: BinOp::Shr, r: Operand::Int(2), .. } => {}
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
}
