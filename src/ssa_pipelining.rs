//! P16. 软件流水 (software pipelining / modulo scheduling) —— 依赖图、MII 与调度器。
//!
//! 本模块只做**分析与调度**（不碰 IR）：把循环体的拼接指令序建成带「距离 + 延迟」的
//! 依赖图，算出迭代间隔下界（RecMII / ResMII），再用 epi-cyclic 模调度找可行 stage。
//!
//! 两条贯穿全模块的原则：
//!
//! 1. **正确性优先于收益**。依赖模型漏一条边的后果是静默错值，不是编译失败，所以
//!    内存操作、以及本 IR 特有的「基名塌缩序」（SSA→AST 重建把 `n_1/n_2` 还原成同一
//!    个变量，故同基名的读/写顺序就是语义）一律保守建边。
//! 2. **不靠推导，靠判定**。`modulo_schedule` 返回的 stage 必须过 `edges_satisfied`
//!    全量校验才算「该间隔可行」；下界被压低是安全问题，被抬高是丢收益，因此
//!    `rec_mii` 只对**闭合环**求最大 Σlatency/Σdistance（把不闭合的长链当环会跳过
//!    本来可行的间隔，见 `high_latency_carried_loop_raises_mii`）。
//!
//! 未做（诚实记录，本模块因此不参与编译产物）：
//!   * 循环识别（从 `Function` 取头/latch/preheader 并判定单块调度区）；
//!   * 把 stage 分配切成 `prologue / kernel×R / epilogue` 实例序列，并证明它覆盖每个
//!     `(节点, 迭代)` 恰好一次；
//!   * 应用到 IR（节点重命名 + 三段块 + 轮转 phi）以及闸门与拒绝诊断；
//!   * 在真实语料上确认存在跨迭代重叠。
//!
//! 上述四项是本方案阶段 2 的剩余工作，判定式自检与依赖图是它们的地基。

use std::collections::{HashMap, HashSet};

use crate::ssa::{BasicBlock, Function, Inst, InstKind, Operand, SsaVar};

// ---------------------------------------------------------------- 资源与延迟模型

/// 单个操作占用的槽类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResKind {
    Alu,
    Flo,
    Load,
    Store,
    /// 调用/内联汇编：占多个周期且不可与外界重排
    Heavy,
}

/// 每行（每个调度槽位）可容纳的各类操作数。
///
/// 本后端是「栈槽 + 单累加器」模型，没有真实端口表；这里按一台双发射标量机建模。
/// 它只影响 II 大小（收益），不影响正确性 —— 正确性完全由 `Edge` 集合与
/// `edges_satisfied` 保证。
#[derive(Clone, Copy, Debug)]
pub struct Resources {
    pub alu: usize,
    pub flo: usize,
    pub load: usize,
    pub store: usize,
    pub heavy: usize,
}

impl Default for Resources {
    fn default() -> Self {
        Resources { alu: 2, flo: 1, load: 1, store: 1, heavy: 1 }
    }
}

fn res_of(k: &InstKind) -> ResKind {
    match k {
        InstKind::Call { .. } | InstKind::Invoke { .. } | InstKind::InlineAsm { .. } => ResKind::Heavy,
        InstKind::Store { .. } | InstKind::IndexStore { .. } | InstKind::MemberStore { .. } => {
            ResKind::Store
        },
        InstKind::Load { .. } | InstKind::IndexLoad { .. } | InstKind::MemberLoad { .. } => {
            ResKind::Load
        },
        InstKind::Bin { l, r, .. }
            if matches!(l, Operand::Float(..)) || matches!(r, Operand::Float(..)) =>
        {
            ResKind::Flo
        },
        _ => ResKind::Alu,
    }
}

/// 产出结果需要的周期数（依赖边上的 latency）。
fn latency_of(k: &InstKind) -> u32 {
    match k {
        InstKind::Call { .. } | InstKind::Invoke { .. } | InstKind::InlineAsm { .. } => 3,
        InstKind::Load { .. } | InstKind::IndexLoad { .. } | InstKind::MemberLoad { .. } => 2,
        _ => 1,
    }
}

/// 需要与其它内存操作定序的指令（含 volatile 读与带初始化式的声明）。
fn is_memory_ordered(k: &InstKind) -> bool {
    matches!(
        k,
        InstKind::Load { .. }
            | InstKind::Store { .. }
            | InstKind::IndexLoad { .. }
            | InstKind::IndexStore { .. }
            | InstKind::MemberLoad { .. }
            | InstKind::MemberStore { .. }
            | InstKind::Call { .. }
            | InstKind::Invoke { .. }
            | InstKind::PreservedDecl { .. }
    ) || matches!(k, InstKind::InlineAsm { volatile: true, .. })
}

// ------------------------------------------------------------------ 操作数遍历

fn kind_uses(k: &InstKind) -> Vec<SsaVar> {
    let mut v: Vec<SsaVar> = Vec::new();
    let push = |o: &Operand, v: &mut Vec<SsaVar>| {
        if let Operand::Var(x) = o {
            v.push(x.clone());
        }
    };
    match k {
        InstKind::Bin { l, r, .. } => {
            push(l, &mut v);
            push(r, &mut v);
        },
        InstKind::Un { e, .. }
        | InstKind::Cast { e, .. }
        | InstKind::Load { e, .. }
        | InstKind::MemberLoad { obj: e, .. } => push(e, &mut v),
        InstKind::Store { e, src, .. } | InstKind::MemberStore { obj: e, src, .. } => {
            push(e, &mut v);
            push(src, &mut v);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            push(arr, &mut v);
            push(idx, &mut v);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            push(arr, &mut v);
            push(idx, &mut v);
            push(src, &mut v);
        },
        InstKind::Call { callee, args, .. } | InstKind::Invoke { callee, args, .. } => {
            push(callee, &mut v);
            for a in args {
                push(a, &mut v);
            }
        },
        InstKind::Select { cond, then_, else_ } => {
            push(cond, &mut v);
            push(then_, &mut v);
            push(else_, &mut v);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for i in inputs {
                push(i, &mut v);
            }
        },
        InstKind::AddrOf { .. } | InstKind::PreservedDecl { .. } | InstKind::EhLandingPad { .. } => {},
    }
    v
}

fn kind_addr_operand(k: &InstKind) -> Option<&Operand> {
    match k {
        InstKind::Store { e, .. } => Some(e),
        InstKind::IndexStore { arr, .. } => Some(arr),
        InstKind::MemberStore { obj, .. } => Some(obj),
        _ => None,
    }
}

// -------------------------------------------------------------------- 依赖图

/// 依赖边：`from` 的结果/位置被 `to` 需要。
///
/// `distance` 是跨迭代距离（0 = 同一迭代内），`latency` 是生产方需要周期数（≥1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub distance: u32,
    pub latency: u32,
}

/// 循环体依赖图。
#[derive(Clone, Debug)]
pub struct DepGraph {
    pub n: usize,
    pub edges: Vec<Edge>,
    pub res: Vec<ResKind>,
    pub lat: Vec<u32>,
}

impl DepGraph {
    pub fn preds(&self, to: usize) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.to == to)
    }
    pub fn succs(&self, from: usize) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.from == from)
    }
}

/// 为一个调度区间（自然循环环上各块指令的拼接序）建依赖图。
///
/// `phi_sources` 把「循环头的某个 phi dest」映射到「在区间内产生它下一值的节点」，
/// 这类读的距离是 1（本次迭代要用上一次迭代算出的值）。
pub fn build_dep_graph(
    nodes: &[Inst],
    func: &Function,
    header: &BasicBlock,
    phi_sources: &HashMap<String, usize>,
) -> DepGraph {
    let n = nodes.len();

    // 用户可见存储的基名：只有它们参与「基名塌缩」，合成临时值 base 全局唯一。
    let mut user_base: HashSet<String> = func.original_locals.iter().cloned().collect();
    for p in &func.params {
        user_base.insert(p.base.clone());
    }
    // AddrOf 解析表：`Store{Var(_taddr)}` 里 `_taddr = AddrOf{x}` → 被写对象 x。
    let mut addr_base: HashMap<String, String> = HashMap::new();
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            if let (Some(d), InstKind::AddrOf { base }) = (&inst.dest, &inst.kind) {
                addr_base.insert(d.name(), base.clone());
            }
        }
    }

    let mut def_at: HashMap<String, usize> = HashMap::new();
    for (i, inst) in nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            def_at.entry(d.name()).or_insert(i);
        }
        if let InstKind::InlineAsm { output_vars, .. } = &inst.kind {
            for v in output_vars {
                def_at.entry(v.name()).or_insert(i);
            }
        }
    }
    let mut phi_at: HashMap<String, usize> = HashMap::new();
    for phi in &header.phis {
        if let Some(&p) = phi_sources.get(&phi.dest.name()) {
            phi_at.insert(phi.dest.name(), p);
        }
    }

    let written_base = |i: usize| -> Option<String> {
        if let Some(d) = &nodes[i].dest {
            return user_base.get(&d.base).map(|_| d.base.clone());
        }
        let a = kind_addr_operand(&nodes[i].kind)?;
        match a {
            Operand::Var(v) => {
                let b = addr_base.get(&v.name())?;
                if user_base.contains(b) {
                    Some(b.clone())
                } else {
                    Some(b.clone())
                }
            },
            _ => None,
        }
    };
    let read_bases = |i: usize| -> Vec<String> {
        kind_uses(&nodes[i].kind)
            .into_iter()
            .filter(|v| user_base.contains(&v.base))
            .map(|v| v.base)
            .collect()
    };

    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<(usize, usize, u32)> = HashSet::new();
    let mut add = |from: usize, to: usize, distance: u32, latency: u32, edges: &mut Vec<Edge>| {
        // 自环只有 distance>0 才是合法依赖：`s = s + 1` 的递归本质就是 from==to、
        // distance=1 的一条边，丢掉它等于抹掉这个循环的串行瓶颈。
        if from == to && distance == 0 {
            return;
        }
        // 去重键含方向：否则会误丢反向边。
        if seen.insert((from, to, distance)) {
            edges.push(Edge { from, to, distance, latency });
        }
    };

    for to in 0..n {
        for uvar in kind_uses(&nodes[to].kind) {
            let name = uvar.name();
            if let Some(&from) = def_at.get(&name) {
                add(from, to, 0, latency_of(&nodes[from].kind), &mut edges);
            }
            if let Some(&from) = phi_at.get(&name) {
                add(from, to, 1, latency_of(&nodes[from].kind), &mut edges);
            }
        }
        // 基名塌缩序：同一基名的读→写 / 写→读 / 写→写 全部按原顺序串行。
        let mine = written_base(to);
        let my_reads = read_bases(to);
        for from in 0..n {
            if from == to {
                continue;
            }
            let need = match &mine {
                Some(b) => {
                    read_bases(from).iter().any(|x| x == b)
                        || written_base(from).as_deref() == Some(b.as_str())
                },
                None => my_reads.iter().any(|b| written_base(from).as_deref() == Some(b.as_str())),
            };
            if need {
                let (a, c) = if from < to { (from, to) } else { (to, from) };
                add(a, c, 0, 1, &mut edges);
            }
        }
        // 内存定序：两个内存操作按区间拼接序串行（无精确别名信息时的唯一诚实判据）。
        if is_memory_ordered(&nodes[to].kind) {
            for from in 0..to {
                if is_memory_ordered(&nodes[from].kind) {
                    add(from, to, 0, latency_of(&nodes[from].kind), &mut edges);
                }
            }
        }
    }
    // 同迭代边的延迟一律取生产方延迟（保守，且与顺序边共用一套判据）。
    for e in edges.iter_mut() {
        if e.distance == 0 {
            e.latency = e.latency.max(latency_of(&nodes[e.from].kind));
        }
    }

    DepGraph {
        n,
        edges,
        res: nodes.iter().map(|i| res_of(&i.kind)).collect(),
        lat: nodes.iter().map(|i| latency_of(&i.kind)).collect(),
    }
}

// ------------------------------------------------------------------------ MII

/// 资源下界 ResMII：各类操作数 ÷ 每行槽数，向上取整。
pub fn res_mii(g: &DepGraph, r: &Resources) -> usize {
    let mut cnt: HashMap<ResKind, usize> = HashMap::new();
    for k in &g.res {
        *cnt.entry(*k).or_insert(0) += 1;
    }
    let need = |k: ResKind, slots: usize| cnt.get(&k).copied().unwrap_or(0).div_ceil(slots.max(1));
    1usize
        .max(need(ResKind::Alu, r.alu))
        .max(need(ResKind::Flo, r.flo))
        .max(need(ResKind::Load, r.load))
        .max(need(ResKind::Store, r.store))
        .max(need(ResKind::Heavy, r.heavy))
}

/// 递归下界 RecMII：图上「延迟和 ÷ 距离和」的最大**闭合环**。
///
/// 必须只算闭合环。用「到任意点的最长带距离路径」当环会把一条不闭合的长链算成
/// 很大的 RecMII，而它被用作 II 搜索起点 ⇒ 会**跳过本来可行的间隔**（实测本仓
/// `s=s+a[i]` 环真值 3，误算成 6，直接错过 ii=3 的可行重叠计划）。下界被抬高不是
/// “多试几个间隔”，而是丢收益；被压低才是安全问题（这里不会：环比例是真下界）。
///
/// 距离预算上限 `MAX_DIST_SUM`：更大的环在本仓语料中不存在，漏算只会让下界偏小
/// （多试几个 II，结果仍由 `edges_satisfied` 逐条校验）。
pub fn rec_mii(g: &DepGraph) -> usize {
    const MAX_DIST_SUM: usize = 6;
    let n = g.n;
    if n == 0 {
        return 1;
    }
    let neg = i64::MIN / 4;
    let mut best_ratio = 1usize;
    for s in 0..n {
        // best[d][v]: 从 s 出发、累计跳迭代距离恰为 d、到达 v 的最长延迟和
        let mut best = vec![vec![neg; n]; MAX_DIST_SUM + 1];
        best[0][s] = 0;
        for d in 0..=MAX_DIST_SUM {
            // 同迭代边的不动点闭包（不消耗距离预算）
            for _ in 0..n + 1 {
                let mut changed = false;
                for e in g.edges.iter().filter(|e| e.distance == 0) {
                    if best[d][e.from] == neg {
                        continue;
                    }
                    let w = best[d][e.from] + e.latency as i64;
                    if w > best[d][e.to] {
                        best[d][e.to] = w;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            // 跳迭代边把本层解推到 d+dist 层
            for e in g.edges.iter().filter(|e| e.distance > 0) {
                let dd = e.distance as usize;
                if d + dd > MAX_DIST_SUM || best[d][e.from] == neg {
                    continue;
                }
                let w = best[d][e.from] + e.latency as i64;
                if w > best[d + dd][e.to] {
                    best[d + dd][e.to] = w;
                }
            }
        }
        for d in 1..=MAX_DIST_SUM {
            if best[d][s] > 0 {
                best_ratio = best_ratio.max((best[d][s] as usize).div_ceil(d));
            }
        }
    }
    best_ratio
}

// ------------------------------------------------------------------------ 调度

/// 解差分约束 `x[to] ≥ x[from] + latency - distance*ii`（且 `x[i] ≥ floor[i]`）的
/// 最小解。Bellman-Ford 式松弛；`n` 轮后仍可松弛 ⇒ 存在正环 ⇒ 该 ii 不可行。
fn earliest_times(g: &DepGraph, ii: usize, floor: &[usize]) -> Option<Vec<usize>> {
    let n = g.n;
    let ii = ii.max(1);
    let mut x: Vec<i64> = floor.iter().map(|&f| f as i64).collect();
    for round in 0..=n {
        let mut changed = false;
        for i in 0..n {
            if x[i] < floor[i] as i64 {
                x[i] = floor[i] as i64;
                changed = true;
            }
        }
        for e in &g.edges {
            let need = x[e.from] + e.latency as i64 - (e.distance as i64) * (ii as i64);
            if need > x[e.to] {
                x[e.to] = need;
                changed = true;
            }
        }
        if !changed {
            return Some(x.iter().map(|&w| w.max(0) as usize).collect());
        }
        if round == n {
            return None;
        }
    }
    None
}

/// 校验一份 stage 分配是否满足图里**每条**边：
/// `stage(to) ≥ stage(from) + latency - distance*ii`。
pub fn edges_satisfied(g: &DepGraph, ii: usize, stage: &[usize]) -> bool {
    let ii = ii.max(1);
    g.edges.iter().all(|e| {
        let rhs = (stage[e.from] + e.latency as usize).saturating_sub(e.distance as usize * ii);
        stage[e.to] >= rhs
    })
}

/// kernel 行数上限：超过它 prologue/epilogue 的开销就盖过收益。
fn stage_budget(ii: usize, n: usize) -> usize {
    2 * ii + n
}

/// epi-cyclic 模调度：给定迭代间隔 `ii` 求每个节点的 stage。
///
/// 两步都是**可判定**的，不依赖碰运气的遍历顺序：
///   1. 差分约束最迟时间（`earliest_times`）；
///   2. 预留表（行 = stage % ii）冲突顺延：放不下就抬高该节点下界并回到 1。
/// 最后必须 `edges_satisfied` 全量校验 —— 这才是「该间隔可行」的定义。
pub fn modulo_schedule(g: &DepGraph, ii: usize, r: &Resources) -> Option<Vec<usize>> {
    let n = g.n;
    if n == 0 {
        return Some(Vec::new());
    }
    let ii = ii.max(1);
    let cap_of = |k: ResKind| match k {
        ResKind::Alu => r.alu,
        ResKind::Flo => r.flo,
        ResKind::Load => r.load,
        ResKind::Store => r.store,
        ResKind::Heavy => r.heavy,
    }
    .max(1);

    let mut floor = vec![0usize; n];
    for _ in 0..(2 * n + 8) {
        let stage = earliest_times(g, ii, &floor)?;
        if stage.iter().copied().max().unwrap_or(0) > stage_budget(ii, n) {
            return None;
        }
        let mut rows: Vec<HashMap<ResKind, usize>> = vec![HashMap::new(); ii];
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (stage[i], i));
        let mut bumped = None;
        for &v in &order {
            let k = g.res[v];
            let used = *rows[stage[v] % ii].get(&k).unwrap_or(&0);
            if used < cap_of(k) {
                *rows[stage[v] % ii].entry(k).or_insert(0) += 1;
            } else {
                bumped = Some(v);
                break;
            }
        }
        let Some(v) = bumped else {
            return edges_satisfied(g, ii, &stage).then_some(stage);
        };
        floor[v] += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinOp, Type};
    use crate::diagnostics::Span;
    use crate::ssa::{Phi, Terminator};

    fn sp() -> Span {
        Span::point("", 0, 0)
    }
    fn v(base: &str, version: u32) -> SsaVar {
        SsaVar { base: base.to_string(), version, ty: Type::Int, span: sp() }
    }
    fn vr(base: &str, version: u32) -> Operand {
        Operand::Var(v(base, version))
    }
    fn ic(i: i64) -> Operand {
        Operand::Int(i as i128)
    }
    fn inst(dest: Option<SsaVar>, kind: InstKind) -> Inst {
        Inst { dest, kind, span: sp() }
    }
    fn bin(dest: SsaVar, l: Operand, op: BinOp, r: Operand) -> Inst {
        inst(Some(dest), InstKind::Bin { op, l, r })
    }
    fn ld(dest: SsaVar, e: Operand) -> Inst {
        inst(Some(dest), InstKind::Load { e, ty: Type::Int, is_volatile: false })
    }
    fn st(e: Operand, src: Operand) -> Inst {
        inst(None, InstKind::Store { e, src, is_volatile: false })
    }
    fn bb(name: &str, phis: Vec<Phi>) -> BasicBlock {
        BasicBlock {
            name: name.to_string(),
            phis,
            insts: Vec::new(),
            term: Terminator::Ret { val: None },
            preds: Vec::new(),
            succs: Vec::new(),
        }
    }
    fn phi(base: &str, version: u32) -> Phi {
        Phi { dest: v(base, version), incoming: vec![(v(base, version - 1), "bb_0".to_string())] }
    }

    /// bb_0 = 循环头（承载 phi），bb_1 = 体（调度区所在）。
    fn func(locals: &[&str]) -> Function {
        let mut f = Function::new("f".to_string(), Type::Int, sp());
        for l in locals {
            f.original_locals.insert(l.to_string());
        }
        f.blocks.insert("bb_0".to_string(), bb("bb_0", Vec::new()));
        f.blocks.insert("bb_1".to_string(), bb("bb_1", Vec::new()));
        f.block_order = vec!["bb_0".to_string(), "bb_1".to_string()];
        f
    }

    fn graph(f: &Function, nodes: &[Inst], src: &[(&str, usize)]) -> DepGraph {
        let mut phi_sources: HashMap<String, usize> = HashMap::new();
        for (k, i) in src {
            phi_sources.insert((*k).to_string(), *i);
        }
        build_dep_graph(nodes, f, &f.blocks["bb_0"], &phi_sources)
    }

    #[test]
    fn no_carried_dependency_gives_ii_1() {
        let f = func(&["a"]);
        let nodes =
            vec![bin(v("t", 1), vr("a", 1), BinOp::Add, ic(1)), bin(v("u", 1), vr("t", 1), BinOp::Add, ic(1))];
        let g = graph(&f, &nodes, &[]);
        assert!(g.edges.iter().all(|e| e.distance == 0), "无携带依赖");
        assert_eq!(rec_mii(&g), 1);
        assert!(modulo_schedule(&g, 1, &Resources::default()).is_some());
    }

    /// 循环携带递归必须落成 distance=1 的自环 —— `s = s + 1` 的串行瓶颈。
    #[test]
    fn carried_recurrence_gets_distance_one_edge() {
        let mut f = func(&["i", "s"]);
        f.blocks.get_mut("bb_0").unwrap().phis = vec![phi("i", 2), phi("s", 2)];
        let nodes = vec![
            bin(v("i", 3), vr("i", 2), BinOp::Add, ic(1)),
            bin(v("s", 3), vr("s", 2), BinOp::Add, ic(1)),
        ];
        let g = graph(&f, &nodes, &[("i.2", 0), ("s.2", 1)]);
        let carried: Vec<(usize, usize, u32)> =
            g.edges.iter().filter(|e| e.distance > 0).map(|e| (e.from, e.to, e.distance)).collect();
        assert_eq!(carried.len(), 2, "两条携带依赖：{carried:?}");
        assert!(carried.iter().all(|&(from, to, _)| from == to), "递归应是自环：{carried:?}");
        assert_eq!(rec_mii(&g), 1, "latency=1 的递归 ⇒ RecMII=1");
    }

    /// 递归下界必须按**闭合环**的「延迟和 ÷ 距离和」计算。
    ///
    /// 这条同时复现我踩过的真缺陷：用「到任意点的最长带距离路径」当环，会把
    /// 不闭合的长链算成很大的 RecMII，而它被用作 II 搜索起点 ⇒ 直接跳过本来
    /// 可行的间隔（丢收益，不是多试几个间隔）。
    #[test]
    fn high_latency_carried_loop_raises_mii() {
        let mut f = func(&["s", "a"]);
        f.blocks.get_mut("bb_0").unwrap().phis = vec![phi("s", 2)];
        let nodes = vec![
            bin(v("p", 1), vr("s", 2), BinOp::Add, ic(1)),
            bin(v("q", 1), vr("p", 1), BinOp::Add, ic(1)),
            bin(v("r", 1), vr("q", 1), BinOp::Add, ic(1)),
            bin(v("s", 3), vr("r", 1), BinOp::Add, ic(1)),
        ];
        let g = graph(&f, &nodes, &[("s.2", 3)]);
        assert_eq!(rec_mii(&g), 4, "环 s_2→p→q→r→s_3→回卷：延迟 4 ÷ 距离 1");
        let r = Resources { alu: 4, ..Default::default() };
        assert!(modulo_schedule(&g, 1, &r).is_none(), "II=1 无法满足 4 周期递归环");
        let s = modulo_schedule(&g, 4, &r).expect("II=4 应可行");
        assert!(edges_satisfied(&g, 4, &s));
    }

    #[test]
    fn res_mii_counts_load_slots() {
        let f = func(&["a"]);
        let nodes = vec![ld(v("x", 1), vr("a", 1)), ld(v("y", 1), vr("a", 2)), ld(v("z", 1), vr("a", 3))];
        let g = graph(&f, &nodes, &[]);
        assert_eq!(res_mii(&g, &Resources { load: 1, ..Default::default() }), 3, "3 次 load ÷ 1 槽");
        assert_eq!(res_mii(&g, &Resources { load: 2, ..Default::default() }), 2, "2 槽时向上取整");
    }

    /// 基名塌缩序是正确性问题：重建 AST 把各版本还原成同一个名字，因此同一
    /// 基名的「读先于写」与「写先于读」都必须有边，调度不得颠倒。
    #[test]
    fn base_collapse_order_is_preserved() {
        let f = func(&["t"]);
        let nodes = vec![
            bin(v("u", 1), vr("t", 1), BinOp::Add, ic(1)),
            bin(v("t", 2), vr("u", 1), BinOp::Add, ic(1)),
        ];
        let g = graph(&f, &nodes, &[]);
        assert!(g.succs(0).any(|e| e.to == 1), "t 的读必须先于 t 的写");
        let s = modulo_schedule(&g, 2, &Resources::default()).expect("II=2 可容纳");
        assert!(edges_satisfied(&g, 2, &s), "调度不得颠倒塌缩序");

        let nodes2 = vec![bin(v("t", 1), vr("x", 1), BinOp::Add, ic(1)), ld(v("y", 1), vr("t", 1))];
        let g2 = graph(&f, &nodes2, &[]);
        assert!(g2.succs(0).any(|e| e.to == 1), "定义 t 必须先于把 t 当地址的装载");
    }

    #[test]
    fn memory_ops_serialize() {
        let f = func(&["a", "t"]);
        let nodes = vec![st(vr("a", 1), ic(0)), ld(v("t", 1), vr("a", 2)), st(vr("a", 1), vr("t", 1))];
        let g = graph(&f, &nodes, &[]);
        for (from, to) in [(0usize, 1usize), (1, 2), (0, 2)] {
            assert!(g.succs(from).any(|e| e.to == to), "内存操作 {from}→{to} 必须定序");
        }
    }

    /// 跨迭代成环（延迟和 > 距离和 × II）时，调度器必须拒绝该间隔而不是给出
    /// 一个违反依赖的 stage。
    #[test]
    fn cyclic_same_iteration_is_refused() {
        let mut f = func(&["x"]);
        f.blocks.get_mut("bb_0").unwrap().phis = vec![phi("x", 1)];
        // 0: t = load x_1（lat 2）  1: x_2 = t + 1；x_1 由节点 1 的上一次迭代提供。
        let nodes = vec![ld(v("t", 1), vr("x", 1)), bin(v("x", 2), vr("t", 1), BinOp::Add, ic(1))];
        let g = graph(&f, &nodes, &[("x.1", 1)]);
        assert_eq!(rec_mii(&g), 3, "环 0→1→0：延迟 2+1 ÷ 距离 1");
        let r = Resources::default();
        assert!(modulo_schedule(&g, 1, &r).is_none(), "II=1 不可满足");
        assert!(modulo_schedule(&g, 2, &r).is_none(), "II=2 仍不可满足");
        let s = modulo_schedule(&g, 3, &r).expect("II=3 可行");
        assert!(edges_satisfied(&g, 3, &s));
    }
}
