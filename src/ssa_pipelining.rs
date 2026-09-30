//! P16. 软件流水 (software pipelining / modulo scheduling) —— 依赖图、MII、调度、切分与闸门。
//!
//! 本模块只做**分析与计划**（不碰 IR）：识别自然循环 → 把调度区建成带「距离 + 延迟」
//! 的依赖图 → 算迭代间隔下界（RecMII / ResMII）→ epi-cyclic 模调度 → 切成
//! `prologue / kernel×R / epilogue` 三段实例序列 → 用一套闸门判收益与代价。
//!
//! 两条贯穿全模块的原则：
//!
//! 1. **正确性优先于收益**。依赖模型漏一条边的后果是静默错值，不是编译失败，所以
//!    内存操作、以及本 IR 特有的「基名塌缩序」（SSA→AST 重建把 `n_1/n_2` 还原成同一
//!    个变量，故同基名的读/写顺序就是语义）一律保守建边。
//! 2. **不靠推导，靠判定**。两道判定缺一不可：
//!    * `modulo_schedule` 返回的 stage 必须过 `edges_satisfied` 全量校验才算「该间隔
//!      可行」；下界被压低是安全问题，被抬高是丢收益，因此 `rec_mii` 只对**闭合环**
//!      求最大 Σlatency/Σdistance（把不闭合的长链当环会跳过本来可行的间隔，见
//!      `high_latency_carried_loop_raises_mii`）。
//!    * `InstancePlan::verify` 要求三段展开的实例序列与绝对时间序
//!      `T(p,k) = stage[p] + k·II` **逐元素相等**，且每个 `(节点, 迭代)` 恰好出现一次。
//!      上一版实现里它当场抓出两个方向性错误（kernel 迭代号没随轮次推进、跳轮引用
//!      偏移符号写反导致所有真实循环被误拒），若无它则会以错值形式进入产物。
//!
//! 未做（诚实记录，本模块因此不参与编译产物）：把计划**应用到 IR** —— 节点重命名、
//! 三段块、一级轮转 phi，接入 `run_ssa_passes` 并用执行夹具与 objdump 验收。
//!
//! 识别层只认真实 IR 的形状，不猜：`for` 的头是带 phi 的 `for.body`、latch 是条件回边
//! 所在的 `for.step`、`i++` 被拆成 `_tpost=i+0; _tupd=_tpost+1; i_2=_tupd+0` 的拷贝链；
//! `while` 的头 `while.cond` 自己带分支。环内成员用自然循环体集合而不是支配关系
//! （循环出口也受头支配）。详见 `docs/ssa-pass-upgrade-plan.md` §7.1。

use std::collections::{HashMap, HashSet};

use crate::ast::BinOp;
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

// ================================================================ 循环识别

/// 一个通过全部准入检查、可以进模调度的循环。
///
/// 拒绝一律带原因：流水的准入是一串保守条件，任何一条误判都会让 pass 默默空转，
/// 而「它没跑」和「它为什么没跑」在产物上完全一样 —— 没有原因清单就只能靠猜。
#[derive(Clone, Debug)]
pub struct PipelinedLoop {
    /// 循环头：带 phi 的块。真实 IR 里 `for` 的头是 `for.body`，**不是** `for.cond`。
    pub header: String,
    /// 唯一 latch（含条件回边的块也允许，只要回边目标只有头一个）。
    pub latch: String,
    /// 区域外唯一前驱。`for` 的 preheader 是 `for.cond`（条件在那里算）。
    pub preheader: String,
    /// 环外出口。准入已保证整个区域指向环外的目标**只有一个**，所它是必填项。
    pub exit: String,
    /// 调度区所在的块序列（头在前），拼接起来就是 `nodes` 的定序真源。
    pub region_bbs: Vec<String>,
    pub nodes: Vec<Inst>,
    /// phi dest 的 SSA 名 → 在区域内产生其「下一值」的节点下标（读它的距离是 1）。
    pub phi_sources: HashMap<String, usize>,
    /// 归纳变量基名（trip count 由它算出）。
    pub iv_base: String,
    /// 编译期已知的迭代次数。
    pub trip: usize,
}

fn succ_names(func: &Function, name: &str) -> Vec<String> {
    // 后继只能从终结符派生：`bb.succs` 是构造期缓存，任何改过 terminator 的
    // pass 之后就是脏数据。
    let mut v = crate::ssa::term_succs_pub(&func.blocks[name].term);
    v.sort();
    v.dedup();
    v
}

fn pred_names(func: &Function, name: &str) -> Vec<String> {
    let mut all = crate::ssa::collect_all_preds(func);
    let mut v = all.remove(name).unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

fn dominates(doms: &HashMap<String, HashSet<String>>, a: &str, b: &str) -> bool {
    doms.get(b).map(|d| d.contains(a)).unwrap_or(false)
}

fn as_var(o: &Operand) -> Option<SsaVar> {
    if let Operand::Var(v) = o {
        Some(v.clone())
    } else {
        None
    }
}

fn as_int(o: &Operand) -> Option<i64> {
    if let Operand::Int(i) = o { Some(*i as i64) } else { None }
}

/// `x = v + 0` / `x = 0 + v` 是构建器的**拷贝惯用法**（用它物化一个值），
/// 绝不允许被当成步长 —— 把它计成一步会让真正的 `+1` 冲突、整条递推被误拒。
fn copy_source(k: &InstKind) -> Option<SsaVar> {
    if let InstKind::Bin { op: BinOp::Add, l, r } = k {
        if let (Some(v), Some(0)) = (as_var(l), as_int(r)) {
            return Some(v);
        }
        if let (Some(0), Some(v)) = (as_int(l), as_var(r)) {
            return Some(v);
        }
    }
    None
}

/// 带非常量偏移的加/减：`v + c` 给出 `c`，`v - c` 给出 `-c`，`c + v` 给出 `c`。
/// `c - v` 不是归纳变量的线性递推，一律不认。
fn stepped_source(k: &InstKind) -> Option<(SsaVar, i64)> {
    let InstKind::Bin { op, l, r } = k else {
        return None;
    };
    if let (Some(v), Some(c)) = (as_var(l), as_int(r)) {
        if c == 0 {
            return None; // 那是拷贝，不是步进（由 copy_source 负责）
        }
        return match op {
            BinOp::Add => Some((v, c)),
            BinOp::Sub => Some((v, -c)),
            _ => None,
        };
    }
    if let (Some(c), Some(v)) = (as_int(l), as_var(r)) {
        if c != 0 && matches!(*op, BinOp::Add) {
            return Some((v, c));
        }
    }
    None
}

/// 从 phi 的 latch 侧值出发沿拷贝链回溯，要求恰好遇到**一次**真实步进、
/// 且链最终回到归纳变量自身。返回步长。
fn iv_step(nodes: &[Inst], def_of: &HashMap<String, usize>, start: &SsaVar, iv: &SsaVar) -> Option<i64> {
    let mut cur = start.clone();
    let mut step: Option<i64> = None;
    for _ in 0..8 {
        if cur.base == iv.base && cur.version == iv.version {
            return step;
        }
        let &i = def_of.get(&cur.name())?;
        let k = &nodes[i].kind;
        if let Some(src) = copy_source(k) {
            cur = src;
            continue;
        }
        let (v, d) = stepped_source(k)?;
        if step.is_some() {
            return None; // 两段真实步进 ⇒ 不是一条线性递推
        }
        step = Some(d);
        cur = v;
    }
    None
}

/// 区域外某个基名的**唯一**常量定义值。
///
/// 必须查全函数（只查 preheader 是上轮的错）：`for (int i = 0; ...)` 的 `i = 0`
/// 落在 `entry`，而 preheader `for.cond` 里只有比较。
fn const_def_outside(func: &Function, region: &[String], base: &str) -> Option<i64> {
    let mut found: Option<i64> = None;
    for name in crate::ssa::ordered_block_names(func) {
        if region.contains(&name) {
            continue;
        }
        let Some(bb) = func.blocks.get(&name) else { continue };
        for inst in &bb.insts {
            let Some(d) = &inst.dest else { continue };
            if d.base != base {
                continue;
            }
            if let InstKind::Bin { op: BinOp::Add, l, r } = &inst.kind {
                if let (Some(a), Some(b)) = (as_int(l), as_int(r)) {
                    if found.is_some() {
                        return None; // 多处常量定义 ⇒ 进循环时的值不确定
                    }
                    found = Some(a + b);
                    continue;
                }
            }
            return None; // 定义不是常量 ⇒ trip 不可静态推出
        }
    }
    found
}

/// 比较运算符左右翻转。
fn flip_cmp(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Gt => BinOp::Lt,
        BinOp::Le => BinOp::Ge,
        BinOp::Ge => BinOp::Le,
        o => o,
    }
}

/// 从一块里取那条控制入环的比较：一边是变量、另一边是常量。
///
/// 返回的布尔为 false 表示变量写在右边（`8 < i`），调用方必须把运算符翻转
/// 再判方向，否则会把一个下降循环当成上升的。
fn loop_bound(func: &Function, blk: &str) -> Option<(SsaVar, BinOp, i64, bool)> {
    let bb = func.blocks.get(blk)?;
    let mut last = None;
    for inst in &bb.insts {
        if let InstKind::Bin { op, l, r } = &inst.kind {
            let cmp = match op {
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => *op,
                _ => continue,
            };
            if let (Some(v), Some(c)) = (as_var(l), as_int(r)) {
                last = Some((v, cmp, c, true));
            } else if let (Some(c), Some(v)) = (as_int(l), as_var(r)) {
                last = Some((v, cmp, c, false));
            }
        }
    }
    last
}

/// 判定一个候选头是否是可流水的循环。
///
/// 方案的三条前置全部在这里把关：单 latch 自然循环、区域是直线链（⇒不嵌套）、
/// 编译期已知 trip count 且归纳变量是线性 `+1`。
pub fn classify_loop(func: &Function, header: &str) -> Result<PipelinedLoop, &'static str> {
    let doms = crate::ssa::compute_dominators(func);
    let bb = func.blocks.get(header).ok_or("块不存在")?;
    if bb.phis.is_empty() {
        return Err("头块没有 phi：不是循环携带形式的环");
    }
    if !dominates(&doms, header, header) {
        return Err("头块不可达");
    }

    // latch = 被头支配的前驱，必须唯一。
    let mut latches: Vec<String> =
        pred_names(func, header).into_iter().filter(|p| dominates(&doms, header, p)).collect();
    latches.dedup();
    if latches.is_empty() {
        return Err("没有回边");
    }
    if latches.len() > 1 {
        return Err("多 latch 循环（需要多入口旋转，本层不支持）");
    }
    let latch = latches.remove(0);

    // 环内成员用**自然循环体**集合，不用支配关系：循环出口同样受头支配（实测
    // `while.end` 的支配集含 `while.cond`），拿支配当环内判据会把出口当成第二个
    // 环内后继，然后把整个 `while` 形态错拒。
    let body: HashSet<String> =
        crate::ssa::find_loop_body(func, header, &latch).into_iter().collect();
    let (region_bbs, mut exits) = walk_region(func, header, &latch, &body)?;
    // 链必须覆盖整个自然循环体：少一个块就意味着环内还有分支（嵌套或 if）。
    if region_bbs.len() != body.len() {
        return Err("链没覆盖整个自然循环体（环内有分支块）");
    }
    // 出口：整个区域指向环外的块只能有一个目标。
    exits.sort();
    exits.dedup();
    if exits.len() != 1 {
        return Err(if exits.is_empty() {
            "循环没有出口（无限循环，trip 无从谈起）"
        } else {
            "循环有多个出口（需要出口侧复制，本层不做）"
        });
    }
    let exit = exits.remove(0);

    // preheader = 区域外的头前驱，必须恰好一个。
    let outside: Vec<String> =
        pred_names(func, header).into_iter().filter(|p| *p != latch).collect();
    if outside.len() != 1 {
        return Err(if outside.is_empty() { "头没有区域外前驱（不可达循环）" } else { "多个区域外前驱（循环被多路进入）" });
    }
    let preheader = outside[0].clone();
    if region_bbs.contains(&preheader) {
        return Err("preheader 落在区域内");
    }

    // 拼接调度区。
    let mut nodes: Vec<Inst> = Vec::new();
    for name in &region_bbs {
        for inst in &func.blocks[name].insts {
            nodes.push(inst.clone());
        }
    }
    if nodes.is_empty() {
        return Err("调度区为空");
    }
    if nodes.len() > 64 {
        return Err("调度区超过 64 条指令（方案的准入上限）");
    }
    let mut def_of: HashMap<String, usize> = HashMap::new();
    for (i, inst) in nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            if def_of.insert(d.name(), i).is_some() {
                // 同一 SSA 名在区域内两次定义 ⇒ 拼接序与单赋值矛盾，绝不能继续。
                return Err("区域内有重复定义（IR 违反单赋值）");
            }
        }
    }

    // phi 的 latch 侧输入必须都在区域内产生。
    let mut phi_sources: HashMap<String, usize> = HashMap::new();
    for p in &bb.phis {
        let Some((src, _)) = p.incoming.iter().find(|(_, from)| *from == latch) else {
            return Err("phi 没有 latch 侧输入");
        };
        let Some(&idx) = def_of.get(&src.name()) else {
            return Err("phi 的 latch 侧值不在调度区内产生");
        };
        phi_sources.insert(p.dest.name(), idx);
    }

    // 归纳变量与 trip count。真正的归纳变量是头的 phi dest；比较里用的
    // 可能是它，也可能是进环前的那个版本（`for` 的 `for.cond` 用 `i_1`）。
    //
    // 比较的位置两种循环形态不同：`for` 在 preheader（`for.cond` 算 `i<8`），
    // `while` 在头块自身。两边都有时不猜哪个控制回边，直接拒。
    let (bound_var, op, limit, var_left) = match (loop_bound(func, header), loop_bound(func, &preheader)) {
        (Some(a), None) => a,
        (None, Some(a)) => a,
        (Some(_), Some(_)) => return Err("头块与 preheader 各有一个入环比较（不猜哪个控制回边）"),
        (None, None) => return Err("入环比较不是「变量 vs 常量」，trip 未知"),
    };
    let op = if var_left { op } else { flip_cmp(op) };
    let iv_base = bound_var.base.clone();
    let init = const_def_outside(func, &region_bbs, &iv_base).ok_or("归纳变量初值不是区域外的唯一常量定义")?;
    let phi_dest = bb
        .phis
        .iter()
        .find(|p| p.dest.base == iv_base)
        .map(|p| p.dest.clone())
        .ok_or("归纳变量在头部没有 phi")?;
    let start = bb
        .phis
        .iter()
        .find(|p| p.dest.base == iv_base)
        .and_then(|p| p.incoming.iter().find(|(_, from)| *from == latch).map(|(s, _)| s.clone()))
        .ok_or("归纳变量没有 latch 侧 phi")?;
    let step = iv_step(&nodes, &def_of, &start, &phi_dest).ok_or("归纳变量不是「沿拷贝链恰好一步」的线性递推")?;
    if step <= 0 {
        return Err("步长非正（本层只旋转上升的归纳变量）");
    }
    let span = match op {
        BinOp::Lt => limit.checked_sub(init).ok_or("trip 计算溢出")?,
        BinOp::Le => limit.checked_sub(init).and_then(|d| d.checked_add(1)).ok_or("trip 计算溢出")?,
        BinOp::Gt | BinOp::Ge => return Err("下降比较（本层只处理上升归纳变量）"),
        _ => return Err("比较运算符异常"),
    };
    if span < 1 {
        return Err("trip count 非正");
    }
    // 步长 >1 时执行次数是向上取整。不整除意味着最后一轮不满，本层接受：
    // 三段展开覆盖的是「迭代实例」，不要求每轮跑等长的步数。
    // （手写上取整：`i64::div_ceil` 在本 toolchain 仍是 unstable。）
    let trip = span.checked_add(step - 1).ok_or("trip 计算溢出")? / step;
    if trip < 1 {
        return Err("trip count 非正");
    }
    Ok(PipelinedLoop {
        header: header.to_string(),
        latch,
        preheader,
        exit,
        region_bbs,
        nodes,
        phi_sources,
        iv_base,
        trip: trip as usize,
    })
}

fn bb_phis_empty(func: &Function, name: &str) -> bool {
    func.blocks.get(name).map(|b| b.phis.is_empty()).unwrap_or(false)
}

/// 从头出发、只沿「属于自然循环体」的后继推进，走到 latch 为止；同时收集
/// 所有指向环外的边。
///
/// 返回 `(区域块链, 环外后继)`。区域必须是直线链：每个块最多一个环内后继，
/// 链尾就是那个带唯一回边的块。`for` 的头 (`for.body`) 只有唯一后继、回边在
/// `for.step`（那里同时出环）；`while` 的头 (`while.cond`) 自己就带分支，一个后继
/// 在环内、另一个是出口 —— 两种形态都走同一条规则，不需要猜形状。
fn walk_region(
    func: &Function,
    header: &str,
    latch: &str,
    body: &HashSet<String>,
) -> Result<(Vec<String>, Vec<String>), &'static str> {
    let mut region_bbs: Vec<String> = vec![header.to_string()];
    let mut exits: Vec<String> = Vec::new();
    loop {
        let cur = region_bbs.last().cloned().unwrap();
        let mut back = 0usize;
        let mut ins: Vec<String> = Vec::new();
        for s in succ_names(func, &cur) {
            if s == header {
                back += 1;
            } else if body.contains(&s) {
                ins.push(s);
            } else {
                exits.push(s);
            }
        }
        if back > 1 {
            return Err("同一块有多条回边");
        }
        if back == 1 {
            // 走到环尾：这个块就是 latch，链上不能再有别的回边或环内分支。
            if cur == *header {
                return Err("头块自环（调度区为空）");
            }
            if !ins.is_empty() {
                return Err("latch 在回边之外还有环内后继（不是直线链）");
            }
            if cur != latch {
                return Err("链上非 latch 块也有回边（内层循环 ⇒ 嵌套）");
            }
            return Ok((region_bbs, exits));
        }
        // 不回头的块：环内后继必须恰好一个（⇒ 直线链，无分支、无嵌套）。
        if ins.len() != 1 {
            return Err("区域不是直线链（环内分支或多入口）");
        }
        if region_bbs.len() >= 3 {
            return Err("区域超过 3 块");
        }
        let nxt = ins.remove(0);
        if region_bbs.contains(&nxt) {
            return Err("区域内成环");
        }
        if !bb_phis_empty(func, &nxt) {
            return Err("区域中间的块带 phi（多进块，需要额外的 phi 修正）");
        }
        region_bbs.push(nxt);
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use crate::ssa::Function;

    fn fn_ssa(src: &str, name: &str) -> Function {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        for decl in m.source().decls.iter() {
            if let crate::ast::Decl::Func(f) = decl {
                if f.name.name == name {
                    return crate::ssa::build_from_func(f).expect("应能 SSA 化");
                }
            }
        }
        panic!("找不到函数 {name}");
    }

    /// 所有带 phi 的块都是候选头（头的判据是「有 phi 且有回边」，不是「是分支块」）。
    fn classify_any(f: &Function) -> Result<PipelinedLoop, Vec<(String, &'static str)>> {
        let mut errs = Vec::new();
        for name in crate::ssa::ordered_block_names(f) {
            if f.blocks[&name].phis.is_empty() {
                continue;
            }
            match classify_loop(f, &name) {
                Ok(l) => return Ok(l),
                Err(e) => errs.push((name, e)),
            }
        }
        errs.push(("（没有带 phi 的块）".to_string(), "无候选头"));
        Err(errs)
    }

    const RED: &str = "int red(int n,int*a){int s=0;for(int i=0;i<8;i++)s+=a[i];return s;}\n";
    const DEC: &str = "int dec(int n){int s=0;while(n>0){s+=n;n--;}return s;}\n";
    const UNKNOWN: &str = "int unknown(int n,int*a){int s=0;for(int i=0;i<n;i++)s+=a[i];return s;}\n";
    const NEST: &str = "int nest(int n){int s=0;for(int i=0;i<4;i++)for(int j=0;j<4;j++)s+=1;return s;}\n";

    #[test]
    fn recognizes_the_real_for_shape() {
        let f = fn_ssa(RED, "red");
        let l = classify_any(&f).unwrap_or_else(|e| panic!("red 应被识别：{e:?}"));
        // 头是带 phi 的 for.body，不是 for.cond；latch 是条件回边所在的 for.step。
        assert_eq!(l.header, "for.body");
        assert_eq!(l.latch, "for.step");
        assert_eq!(l.preheader, "for.cond");
        assert_eq!(l.exit, "for.end");
        assert_eq!(l.region_bbs, vec!["for.body".to_string(), "for.step".to_string()]);
        assert_eq!(l.nodes.len(), 7, "区域拼接序 = 2 + 5");
        assert_eq!(l.iv_base, "i");
        assert_eq!(l.trip, 8);
        // 每个 phi 的 latch 侧值都必须在区域内产生，且 phi_sources 指向它。
        for p in &f.blocks["for.body"].phis {
            let idx = *l
                .phi_sources
                .get(&p.dest.name())
                .unwrap_or_else(|| panic!("phi {} 没有 phi_sources", p.dest.name()));
            let d = l.nodes[idx].dest.as_ref().unwrap();
            let want = p.incoming.iter().find(|(_, from)| *from == l.latch).unwrap().0.clone();
            assert_eq!(d.name(), want.name(), "phi_sources 必须指向 latch 侧生产者");
        }
    }

    /// `while` 的头自带分支（一个后继在环内、一个是出口）。旧版用「受头支配」
    /// 判环内，而出口也受头支配 ⇒ 两个后继都被当成环内后继 ⇒ 整类 `while` 错拒。
    /// 现在它走链能过，并在「初值不是常量」这道闸上被正确拦下（n 来自参数，
    /// trip 取决于运行期值）。
    #[test]
    fn recognizes_while_shape_and_refuses_runtime_iv() {
        let f = fn_ssa(DEC, "dec");
        let errs = classify_any(&f).expect_err("n 来自参数，trip 编译期未知");
        for (name, e) in errs.iter() {
            assert!(
                !e.contains("直线链") && !e.contains("多入口"),
                "while 的头不该再被「唯一后继」假设错拒：{name} ⇒ {e}"
            );
        }
        assert!(errs.iter().any(|(_, e)| e.contains("唯一常量定义")), "{errs:?}");
    }

    /// 初值是常量时才能走到步长判据：`while(n>0) n=n-1` 的步长是 -1，必拒。
    #[test]
    fn refuses_nonpositive_step() {
        let src = "int dec2(int m){int n=8,s=0;while(n>0){s+=n;n=n-1;}return s+m;}\n";
        let f = fn_ssa(src, "dec2");
        let errs = classify_any(&f).expect_err("下降归纳变量不该被旋转");
        assert!(errs.iter().any(|(_, e)| *e == "步长非正（本层只旋转上升的归纳变量）"), "{errs:?}");
    }

    /// 翻转逻辑的两面都要钉住：`0 < i` 等价于 `i > 0`（下降）⇒ 必须被拒；
    /// `8 > i` 等价于 `i < 8`（上升）⇒ 必须被接受且 trip 正确。少了后者，
    /// 「变量写右边就拒」也能把测试做绿，但那会把一批可流水循环默默丢掉。
    #[test]
    fn flipped_comparison_keeps_direction_semantics() {
        // `0 < i`：变量在右，翻转后是 Gt ⇒ 下降，拒。
        let src_down = "int d(int n,int*a){int s=0;for(int i=0;0<i;i++)s+=a[i];return s;}\n";
        let f = fn_ssa(src_down, "d");
        let errs = classify_any(&f).expect_err("0<i 等价于 i>0，是下降循环");
        assert!(errs.iter().any(|(_, e)| *e == "下降比较（本层只处理上升归纳变量）"), "{errs:?}");

        // `8 > i`：变量在右，翻转后是 Lt ⇒ 上升，收，trip=4（ceil(8/2)）。
        let src_up = "int u(int n,int*a){int s=0;for(int i=0;8>i;i+=2)s+=a[i];return s;}\n";
        let f = fn_ssa(src_up, "u");
        let l = classify_any(&f).unwrap_or_else(|e| panic!("8>i 等价于 i<8，应被接受：{e:?}"));
        assert_eq!(l.trip, 4);
    }

    #[test]
    fn refuses_an_unknown_trip_count() {
        let f = fn_ssa(UNKNOWN, "unknown");
        let errs = classify_any(&f).expect_err("i<n 的 trip 编译期未知");
        assert!(errs.iter().any(|(_, e)| e.contains("trip 未知")), "{errs:?}");
    }

    #[test]
    fn nested_inner_loop_is_a_chain_but_outer_is_not() {
        let f = fn_ssa(NEST, "nest");
        // 内层 for.body 仍是直线链；外层区域的链上会出现带 phi 的块 ⇒ 必须被拒。
        let mut ok_inner = 0;
        let mut refused: Vec<(String, &'static str)> = Vec::new();
        for name in crate::ssa::ordered_block_names(&f) {
            if f.blocks[&name].phis.is_empty() {
                continue;
            }
            match classify_loop(&f, &name) {
                Ok(l) => {
                    assert_eq!(l.nodes.len(), 6, "内层区域 = for.body(1)+for.step(5)：{l:?}");
                    assert_eq!(l.trip, 4);
                    ok_inner += 1;
                },
                Err(e) => refused.push((name, e)),
            }
        }
        assert_eq!(ok_inner, 1, "只应识别出最内层：{refused:?}");
        assert_eq!(refused.len(), 1, "外层必须被拒且给出原因：{refused:?}");
        assert!(refused[0].1.contains("直线链") || refused[0].1.contains("带 phi"), "{refused:?}");
    }

    /// 识别结果直接喂调度层：真实归约循环必须给出**跳迭代重叠**（depth ≥ 1）。
    ///
    /// 这条断言就是 P16 值得落地的依据：没有重叠的流水只是把指令搬个位置。
    /// 它同时钉住 RecMII 的真值 3（load 延迟 2 + 加 1）——把不闭合长链当环的
    /// 错算法会算出 6，于是这个可行的 ii=3 会被搜索起点直接跳过。
    #[test]
    fn recognized_loop_has_real_cross_iteration_overlap() {
        let f = fn_ssa(RED, "red");
        let l = classify_any(&f).unwrap_or_else(|e| panic!("{e:?}"));
        let g = build_dep_graph(&l.nodes, &f, &f.blocks[&l.header], &l.phi_sources);
        let r = Resources::default();
        assert_eq!(rec_mii(&g), 3, "归约环的 RecMII 应为 3");
        let ii = rec_mii(&g).max(res_mii(&g, &r));
        let st = modulo_schedule(&g, ii, &r).expect("MII 处必须可行");
        assert!(edges_satisfied(&g, ii, &st), "调度结果必须逐边满足");
        let depth = st.iter().copied().max().unwrap_or(0) / ii;
        assert!(depth >= 1, "真实归约循环必须给出跳迭代重叠：ii={ii} stage={st:?}");
        // trip 必须足够长才值得旋转（闸门在下一步统一把关）。
        assert!(l.trip >= 2 * ii, "trip={} ii={}", l.trip, ii);
    }

    /// 步长 2 的上升归纳变量是可流水的，trip 向上取整（i=0,2,4,6 ⇒ 4 次）。
    #[test]
    fn accepts_step_two_with_ceiled_trip() {
        let src = "int step2(int n,int*a){int s=0;for(int i=0;i<7;i+=2)s+=a[i];return s;}\n";
        let f = fn_ssa(src, "step2");
        let l = classify_any(&f).unwrap_or_else(|e| panic!("步长 2 应被接受：{e:?}"));
        assert_eq!(l.iv_base, "i");
        assert_eq!(l.trip, 4, "ceil(7/2)");
    }

    /// 非线性递推（`i = i*2`）必须被拒：沿拷贝链找不到「恰好一次加法步进」。
    #[test]
    fn refuses_nonlinear_induction() {
        let src = "int nonlin(int n,int*a){int s=0;for(int i=1;i<8;i=i*2)s+=a[i];return s;}\n";
        let f = fn_ssa(src, "nonlin");
        let errs = classify_any(&f).expect_err("i=i*2 不是线性归纳变量");
        assert!(errs.iter().any(|(_, e)| e.contains("线性递推")), "{errs:?}");
    }
}

// =============================================== 三段切分与实例覆盖证明

/// `prologue / kernel×R / epilogue` 的展开结果。
///
/// 唯一真源是绝对时间 `T(p,k) = stage[p] + k·II`：`edges_satisfied` 已保证「按 T
/// 序串行执行」满足图上每条边（含跳迭代边），所以只要三段展开出来的实例序列与
/// T 序**逐元素相等**，正确性就从调度层继承过来，不需要再证一次。
#[derive(Clone, Debug)]
pub struct InstancePlan {
    pub ii: usize,
    /// 最大 stage 偏移（= 需要的轮转寄存器级数）。
    pub depth: usize,
    /// 每个节点的轮转偏移：kernel 第 r 轮执行该节点的迭代 `r + shift[p]`。
    pub shift: Vec<usize>,
    /// kernel 的每一行：节点下标按行内 T 序（`stage%ii` 小者先）。
    pub kernel: Vec<Vec<usize>>,
    /// 轮开始前必须先跑的实例（节点, 迭代）。
    pub prologue: Vec<(usize, usize)>,
    /// 轮结束后用来排空的实例（节点, 迭代）。
    pub epilogue: Vec<(usize, usize)>,
}

impl InstancePlan {
    /// 三段拼成的实际执行序列。
    pub fn flatten(&self) -> Vec<(usize, usize)> {
        let mut out = self.prologue.clone();
        for (r, row) in self.kernel.iter().enumerate() {
            for &p in row {
                out.push((p, r + self.shift[p]));
            }
        }
        out.extend(self.epilogue.iter().copied());
        out
    }

    /// 绝对时间序（平局尺子：先 T，再行内位置，再节点号）。
    fn key(&self, stage: &[usize], x: &(usize, usize)) -> (usize, usize, usize) {
        (stage[x.0] + x.1 * self.ii, stage[x.0] % self.ii, x.0)
    }

    /// 证明这份切分是合法的：**每个 (节点, 迭代) 恰好执行一次**，且展开序与 T 序
    /// 逐元素相等。
    ///
    /// 这条自检不是装饰。上一版实现里它当场抓出两个方向性错误：kernel 行把迭代号
    /// 一同复制（每轮跑同一批迭代）、跳轮引用偏移符号写反（所有真实循环被误拒）。
    /// 两者都不会让编译失败，只会让产物错值。
    pub fn verify(&self, stage: &[usize]) -> bool {
        let n = self.shift.len();
        if stage.len() != n || self.kernel.iter().any(|r| r.len() != n) {
            return false;
        }
        let trip = self.kernel.len() + self.depth;
        if trip < 1 || self.ii < 1 {
            return false;
        }
        // 1. 覆盖性：每个 (p,k) 出现且仅出现一次。
        let mut seen = vec![0u8; n.checked_mul(trip).unwrap_or(usize::MAX)];
        let got = self.flatten();
        for &(p, k) in &got {
            if p >= n || k >= trip {
                return false;
            }
            let i = p * trip + k;
            if seen[i] != 0 {
                return false;
            }
            seen[i] = 1;
        }
        if seen.iter().any(|&c| c == 0) {
            return false;
        }
        // 2. 序恒等：与 T 序逐元素相等（不只是「非降」，而是同一个序列）。
        let mut want: Vec<(usize, usize)> =
            (0..n).flat_map(|p| (0..trip).map(move |k| (p, k))).collect();
        want.sort_by_key(|x| self.key(stage, x));
        want.iter().eq(got.iter())
    }
}

/// 把 stage 分配切成三段。找不到合法切分（轮数非正等）时返回 `None`。
pub fn instance_plan(stage: &[usize], ii: usize, trip: usize) -> Option<InstancePlan> {
    let n = stage.len();
    if n == 0 || ii < 1 || trip < 1 {
        return None;
    }
    let delta: Vec<usize> = stage.iter().map(|s| s / ii).collect();
    let depth = *delta.iter().max()?;
    // shift = depth - delta ⇒ 非负，且 kernel 第 r 行的实例都落在时间片
    // [r·II + depth·II, (r+1)·II + depth·II) 内，行内序 = stage%ii 序。
    let shift: Vec<usize> = delta.iter().map(|d| depth - d).collect();
    if trip <= depth {
        return None; // 轮数非正：prologue/epilogue 会盖过 kernel
    }
    let rounds = trip - depth;
    let mut row: Vec<usize> = (0..n).collect();
    row.sort_by_key(|&p| (stage[p] % ii, p));
    let kernel = vec![row; rounds];
    let mut prologue: Vec<(usize, usize)> = Vec::new();
    let mut epilogue: Vec<(usize, usize)> = Vec::new();
    for p in 0..n {
        for k in 0..trip {
            if k < shift[p] {
                prologue.push((p, k));
            } else if k >= shift[p] + rounds {
                epilogue.push((p, k));
            }
        }
    }
    // 两段各自按绝对时间归位（kernel 行内序已由 row 的排序给出）。不排就凑不出
    // T 序，下面的逐元素相等会直接拒收 —— 这正是自检该做的事。
    let tkey = |x: &(usize, usize)| (stage[x.0] + x.1 * ii, stage[x.0] % ii, x.0);
    prologue.sort_by_key(tkey);
    epilogue.sort_by_key(tkey);
    let plan = InstancePlan { ii, depth, shift, kernel, prologue, epilogue };
    // 覆盖完整性与序恒等全交给 verify：它不通过就不返回计划，而不是返回一个
    // 「看起来对」的切分。
    plan.verify(stage).then_some(plan)
}

#[cfg(test)]
mod instance_tests {
    use super::*;

    /// stage=[1,0]、II=1、trip=3 的期望切分（手工按 T(p,k)=stage[p]+k·II 算出）：
    /// T 序 = (1,0)=0, (0,0)=1, (1,1)=1, (0,1)=2, (1,2)=2, (0,2)=3。
    fn rotated() -> InstancePlan {
        instance_plan(&[1, 0], 1, 3).expect("该切分应存在")
    }

    #[test]
    fn no_rotation_yields_pure_kernel() {
        let p = instance_plan(&[0], 1, 3).expect("纯 kernel 切分应存在");
        assert_eq!(p.depth, 0);
        assert_eq!(p.shift, vec![0]);
        assert!(p.prologue.is_empty() && p.epilogue.is_empty());
        assert_eq!(p.kernel.len(), 3);
        assert_eq!(p.flatten(), vec![(0, 0), (0, 1), (0, 2)]);
        assert!(p.verify(&[0]));
    }

    #[test]
    fn rotation_case_covers_every_instance_once() {
        let p = rotated();
        assert_eq!(p.depth, 1);
        assert_eq!(p.shift, vec![0, 1]);
        assert_eq!(p.prologue, vec![(1, 0)], "轮开始前先灌入节点 1 的第 0 轮");
        assert_eq!(p.epilogue, vec![(0, 2)], "轮结束后排空节点 0 的最后一轮");
        assert_eq!(
            p.flatten(),
            vec![(1, 0), (0, 0), (1, 1), (0, 1), (1, 2), (0, 2)]
        );
        assert!(p.verify(&[1, 0]));
    }

    /// 负向对照：少一个实例必须被拒。没有这条，「覆盖完整」的断言可能是空判。
    #[test]
    fn verify_rejects_missing_instance() {
        let mut p = rotated();
        p.epilogue.clear();
        assert!(!p.verify(&[1, 0]), "漏掉排空实例必须被拒");
        let mut q = rotated();
        q.prologue.pop();
        assert!(!q.verify(&[1, 0]), "漏掉灌入实例必须被拒");
    }

    /// 负向对照：行内序颠倒必须被拒（颠倒会违反图上的边）。
    #[test]
    fn verify_rejects_wrong_row_order() {
        let mut p = rotated();
        p.kernel[0] = vec![1, 0];
        assert!(!p.verify(&[1, 0]), "行内序与 T 序不一致必须被拒");
    }

    #[test]
    fn verify_rejects_shift_contradiction() {
        // shift 与 stage 的推导关系被破坏（这里把 shift 改成全 0）⇒ 覆盖会重复。
        let mut p = rotated();
        p.shift = vec![0, 0];
        assert!(!p.verify(&[1, 0]), "轮转偏移与 kernel 迭代号必须自洽");
    }

    /// 真实循环（`red`：II=3、7 节点、trip=8）的切分必须存在、自洽且覆盖全部实例。
    #[test]
    fn plan_from_recognized_loop_is_sliceable() {
        let src = "int red(int n,int*a){int s=0;for(int i=0;i<8;i++)s+=a[i];return s;}\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let mut done = false;
        for decl in m.source().decls.iter() {
            let crate::ast::Decl::Func(f) = decl else { continue };
            if f.name.name != "red" {
                continue;
            }
            let ssa = crate::ssa::build_from_func(f).unwrap();
            let l = classify_loop(&ssa, "for.body").expect("red 应被识别");
            let g = build_dep_graph(&l.nodes, &ssa, &ssa.blocks["for.body"], &l.phi_sources);
            let r = Resources::default();
            let ii = rec_mii(&g).max(res_mii(&g, &r));
            let stage = modulo_schedule(&g, ii, &r).expect("MII 处必须可行");
            let plan = instance_plan(&stage, ii, l.trip).expect("真实循环必须能切三段");
            assert!(plan.verify(&stage), "切分必须与 T 序逐元素相等");
            assert_eq!(plan.flatten().len(), l.nodes.len() * l.trip, "实例覆盖数 = 节点数 × 迭代数");
            assert_eq!(plan.kernel.len() + plan.depth, l.trip);
            done = true;
        }
        assert!(done, "测试没跑到 red");
    }
}

// ============================================================== 统一闸门

/// 一个循环的完整流水计划（识别 + 调度 + 三段切分）。
#[derive(Clone, Debug)]
pub struct PipelinePlan {
    pub lp: PipelinedLoop,
    pub ii: usize,
    pub stage: Vec<usize>,
    pub inst: InstancePlan,
}

/// 收益与代价判据。单独拆出来是为了它能被精确覆盖：II 被资源约束抬高时，
/// 「没有携带依赖」的循环也会出现 depth ≥ 1，拿真实 IR 反推这条判据会得到错结论。
fn benefit_verdict(depth: usize, ii: usize, trip: usize) -> Result<(), &'static str> {
    if depth == 0 {
        return Err("无跳迭代重叠 (depth=0)：流水只是搬动指令");
    }
    if depth > 1 {
        return Err("需要超过一级轮转寄存器（本层不做）");
    }
    if trip < 2 * ii {
        return Err("trip 太短：prologue/epilogue 开销盖过收益");
    }
    Ok(())
}

/// 唯一闸门：识别 → 建图 → MII → 模调度 → 三段切分 → 收益与代价判据。
///
/// 必须只有一条路径。分成两条（比如诊断走一套、实跑走另一套）的后果就是
/// 「测试绿而实际跳过」，或反过来在实际里跑了一个测试没覆盖的宽松版本。
///
/// 代价判据：本层只做**一级**轮转寄存器（`depth ≤ 1`），因为落地的形式是头块
/// 上一对 phi（上一轮 / 本轮）。`depth` 更大时要 N 级轮转值，收益也撑不起改 IR
/// 的风险，直接拒并给原因。
pub fn plan_for_header_with(
    func: &Function,
    header: &str,
    res: &Resources,
) -> Result<PipelinePlan, &'static str> {
    let lp = classify_loop(func, header)?;
    let g = build_dep_graph(&lp.nodes, func, &func.blocks[header], &lp.phi_sources);
    let mii = rec_mii(&g).max(res_mii(&g, res));
    // 从 MII 起逐步增大；再大到节点数就没有重叠可言了。
    let ceiling = mii.max(lp.nodes.len());
    for ii in mii..=ceiling {
        let Some(stage) = modulo_schedule(&g, ii, res) else {
            continue;
        };
        let depth = stage.iter().copied().max().unwrap_or(0) / ii;
        let inst = match instance_plan(&stage, ii, lp.trip) {
            Some(p) => p,
            // 切不出来（轮数非正等）就换下一个间隔继续试：这不算拒绝理由。
            None => continue,
        };
        if let Err(e) = benefit_verdict(depth, ii, lp.trip) {
            // depth=0 这种情形换更大的 II 只会更没意义，直接结束搜索并报原因。
            return Err(e);
        }
        return Ok(PipelinePlan { lp, ii, stage, inst });
    }
    Err("MII 到节点数之间找不到可行调度")
}

/// 用默认资源模型跑闸门。
pub fn plan_for_header(func: &Function, header: &str) -> Result<PipelinePlan, &'static str> {
    plan_for_header_with(func, header, &Resources::default())
}

/// 函数里所有可流水循环的计划（按块名序遍历，保证可复现）。
pub fn plans_for(func: &Function, res: &Resources) -> Vec<PipelinePlan> {
    let mut out = Vec::new();
    for name in crate::ssa::ordered_block_names(func) {
        if func.blocks[&name].phis.is_empty() {
            continue;
        }
        if let Ok(p) = plan_for_header_with(func, &name, res) {
            out.push(p);
        }
    }
    out
}

/// 逐个候选头报告「为什么没被选中」。
///
/// 流水的准入是一串保守条件，任何一条误判都会让 pass 默默空转；没有这个入口
/// 就只能靠猜。本轮的循环识别缺陷（支配关系当环内判据、拷贝链步长、比较方向）
/// 全部是这个入口给出的原因定位的。
pub fn rejections(func: &Function, res: &Resources) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for name in crate::ssa::ordered_block_names(func) {
        if func.blocks[&name].phis.is_empty() {
            continue;
        }
        if let Err(e) = plan_for_header_with(func, &name, res) {
            out.push((name, e));
        }
    }
    out
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::ssa::Function;

    fn fn_ssa(src: &str, name: &str) -> Function {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        for decl in m.source().decls.iter() {
            if let crate::ast::Decl::Func(f) = decl {
                if f.name.name == name {
                    return crate::ssa::build_from_func(f).expect("应能 SSA 化");
                }
            }
        }
        panic!("找不到函数 {name}");
    }

    /// 归约循环：闸门必须给出 ii=3、一级轮转、且覆盖全部实例的计划。
    #[test]
    fn gate_accepts_the_load_reduction() {
        let src = "int red(int n,int*a){int s=0;for(int i=0;i<8;i++)s+=a[i];return s;}\n";
        let f = fn_ssa(src, "red");
        let p = plan_for_header(&f, "for.body").unwrap_or_else(|e| panic!("归约循环应通过闸门：{e}"));
        assert_eq!(p.ii, 3);
        assert_eq!(p.inst.depth, 1);
        assert_eq!(p.inst.kernel.len() + p.inst.depth, p.lp.trip);
        assert_eq!(p.inst.flatten().len(), p.lp.nodes.len() * p.lp.trip);
        // 计划与图必须自洽：逐边校验。
        let g = build_dep_graph(&p.lp.nodes, &f, &f.blocks["for.body"], &p.lp.phi_sources);
        assert!(edges_satisfied(&g, p.ii, &p.stage));
        // 同一入口给出的原因清单里不能再有 for.body。
        assert!(rejections(&f, &Resources::default()).into_iter().all(|(n, _)| n != "for.body"));
    }

    /// 分类器必须认得出「体内只写内存、没有标量递推」这类循环：它的结构（单 latch、
    /// 直线链、常量 trip）完全合法。旧版拿支配关系当环内判据、拿「头只有唯一后继」
    /// 当形状假设，会把整类循环默默丢掉。
    #[test]
    fn classifier_recognizes_pure_store_loop() {
        let src = "int wr(int n,int*a){for(int i=0;i<8;i++)a[i]=i;return n;}\n";
        let f = fn_ssa(src, "wr");
        let lp = classify_loop(&f, "for.body").unwrap_or_else(|e| panic!("纯写循环的结构合法，分类器不该拒：{e}"));
        assert_eq!(lp.trip, 8);
        // 如果闸门接受，计划必须自洽（逐边满足 + 实例覆盖完整）。
        for p in plans_for(&f, &Resources::default()) {
            let g = build_dep_graph(&p.lp.nodes, &f, &f.blocks[&p.lp.header], &p.lp.phi_sources);
            assert!(edges_satisfied(&g, p.ii, &p.stage));
            assert!(p.inst.verify(&p.stage));
        }
    }

    /// 收益判据的三条都要能单独挨到，否则闸门可能是默默空转。
    #[test]
    fn benefit_verdict_covers_each_rule() {
        assert_eq!(benefit_verdict(0, 3, 8), Err("无跳迭代重叠 (depth=0)：流水只是搬动指令"));
        assert_eq!(benefit_verdict(2, 3, 8), Err("需要超过一级轮转寄存器（本层不做）"));
        assert_eq!(benefit_verdict(1, 3, 5), Err("trip 太短：prologue/epilogue 开销盖过收益"));
        assert!(benefit_verdict(1, 3, 6).is_ok());
    }

    /// trip 太短（少于两个 II）时收益不抵 prologue/epilogue 的代价。
    #[test]
    fn gate_refuses_too_short_trip() {
        let src = "int tiny(int n,int*a){int s=0;for(int i=0;i<2;i++)s+=a[i];return s;}\n";
        let f = fn_ssa(src, "tiny");
        let e = plan_for_header(&f, "for.body").expect_err("trip=2 不该流水");
        assert!(e.contains("trip 太短") || e.contains("无跳迭代重叠"), "{e}");
    }

    /// 闸门与诊断共用同一条路径：两者对同一个头必须给出一致的结论。
    #[test]
    fn gate_and_diagnostics_agree() {
        let src = "int red(int n,int*a){int s=0;for(int i=0;i<8;i++)s+=a[i];return s;}\n\
                   int tiny(int n,int*a){int s=0;for(int i=0;i<2;i++)s+=a[i];return s;}\n";
        for name in ["red", "tiny"] {
            let f = fn_ssa(src, name);
            let rej: Vec<String> = rejections(&f, &Resources::default()).into_iter().map(|(b, _)| b).collect();
            let got: Vec<String> = plans_for(&f, &Resources::default()).into_iter().map(|p| p.lp.header.clone()).collect();
            for b in crate::ssa::ordered_block_names(&f) {
                if f.blocks[&b].phis.is_empty() {
                    continue;
                }
                assert!(
                    got.contains(&b) != rej.contains(&b),
                    "{name}/{b}: 接受集合与拒绝集合必须互补：got={got:?} rej={rej:?}"
                );
            }
        }
    }
}
