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
//! 2. **不靠推导，靠判定**。三道判定缺一不可：
//!    * `modulo_schedule` 返回的 stage 必须过 `edges_satisfied` 全量校验才算「该间隔
//!      可行」；下界被压低是安全问题，被抬高是丢收益，因此 `rec_mii` 只对**闭合环**
//!      求最大 Σlatency/Σdistance（把不闭合的长链当环会跳过本来可行的间隔，见
//!      `high_latency_carried_loop_raises_mii`）。
//!    * `InstancePlan::verify` 要求三段展开的实例序列与绝对时间序
//!      `T(p,k) = stage[p] + k·II` **逐元素相等**，且每个 `(节点, 迭代)` 恰好出现一次。
//!      上一版实现里它当场抓出两个方向性错误（kernel 迭代号没随轮次推进、跳轮引用
//!      偏移符号写反导致所有真实循环被误拒），若无它则会以错值形式进入产物。
//!    * `emitted_reads_match` 沿**实际发射序**模拟存储语义：默认按「同基名共享一个槽」
//!      读（重建会把 `e.4`/`e.5`/`e.14` 塌缩成同一个变量，于是「上一轮的值能不能拿到」
//!      取决于行内先后 —— 那是 T 序模型里不存在的**反依赖**，见「发射序」一节）；
//!      而当某个跳轮读有自己的**轮转格**（`InstancePlan::cells`）时，它读的是那个格子
//!      而不是生产者的槽，行内反依赖约束随之消失。
//!
//! 接入状态：已在 `-O2`/`-O3` **默认接入**（`crate::ssa::passes::run_with_opts` 开头，必须在其它
//! 改写循环的 pass 之前跑）；`DCC_SSA_OFF=pipe` 关整条流水，`DCC_SSA_OFF=rot` 只关真轮转
//! （逐字退回「同基名只能有一个槽」的旧保守判据，反向对照靠它）；`DCC_PIPE_DUMP=1`
//! 看计划、拒因与格子表（`rot_cells=`）。
//!
//! 已完成（Phase 6.3）：**真轮转寄存器**。旧版在 `benefit_verdict` 里拿 `depth > 1` 整条
//! 拒收「同一个基名要同时保留两份值」的循环；现在每个 (跳轮生产者, lag) 由
//! `crate::ssa::location::LocMap::reserve_split_name`（与版本分裂同一个占名分配器）分一个
//! 独立基名，它 own 一个 AST 局部、因而 own 一个自己的家，kernel 行尾有一条 lag 从大到小
//! 的拷贝链。代价上限是 `MAX_ROT_DEPTH`/`MAX_ROT_CELLS`；超上限、类型不可拷贝、或发射序
//! 自检仍对不上时继续**拒收**而不是错跑。
//!
//! 识别层只认真实 IR 的形状，不猜：`for` 的头是带 phi 的 `for.body`、latch 是条件回边
//! 所在的 `for.step`、`i++` 被拆成 `_tpost=i+0; _tupd=_tpost+1; i_2=_tupd+0` 的拷贝链；
//! `while` 的头 `while.cond` 自己带分支。环内成员用自然循环体集合而不是支配关系
//! （循环出口也受头支配）。环内成员的定义与现状见 `docs/architecture.md` 的 P16 行。

use std::collections::{HashMap, HashSet};

use crate::ast::BinOp;
use crate::ssa::{BasicBlock, Function, Inst, InstKind, Operand, Phi, SsaVar};

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
        Resources {
            alu: 2,
            flo: 1,
            load: 1,
            store: 1,
            heavy: 1,
        }
    }
}

fn res_of(k: &InstKind) -> ResKind {
    match k {
        InstKind::Call { .. } | InstKind::Invoke { .. } | InstKind::InlineAsm { .. } => {
            ResKind::Heavy
        },
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
        | InstKind::MemberLoad { obj: e, .. }
        | InstKind::MemberAddr { obj: e, .. } => push(e, &mut v),
        InstKind::Store { e, src, .. } | InstKind::MemberStore { obj: e, src, .. } => {
            push(e, &mut v);
            push(src, &mut v);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            push(arr, &mut v);
            push(idx, &mut v);
        },
        // `IndexAddr` 与 `IndexLoad` 同样读 arr/idx，只是它不访问内存（见
        // `res_of`/`latency_of`/`is_memory_ordered` 的兜底：地址算术计为 Alu/1/不定序）。
        InstKind::IndexAddr { arr, idx, .. } => {
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
        InstKind::AddrOf { .. }
        | InstKind::PreservedDecl { .. }
        | InstKind::EhLandingPad { .. } => {},
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
            edges.push(Edge {
                from,
                to,
                distance,
                latency,
            });
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
                None => my_reads
                    .iter()
                    .any(|b| written_base(from).as_deref() == Some(b.as_str())),
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
    let cap_of = |k: ResKind| {
        match k {
            ResKind::Alu => r.alu,
            ResKind::Flo => r.flo,
            ResKind::Load => r.load,
            ResKind::Store => r.store,
            ResKind::Heavy => r.heavy,
        }
        .max(1)
    };

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
        SsaVar {
            base: base.to_string(),
            version,
            ty: Type::Int,
            span: sp(),
        }
    }
    fn vr(base: &str, version: u32) -> Operand {
        Operand::Var(v(base, version))
    }
    fn ic(i: i64) -> Operand {
        Operand::Int(i as i128)
    }
    fn inst(dest: Option<SsaVar>, kind: InstKind) -> Inst {
        Inst {
            dest,
            kind,
            span: sp(),
        }
    }
    fn bin(dest: SsaVar, l: Operand, op: BinOp, r: Operand) -> Inst {
        inst(Some(dest), InstKind::Bin { op, l, r })
    }
    fn ld(dest: SsaVar, e: Operand) -> Inst {
        inst(
            Some(dest),
            InstKind::Load {
                e,
                ty: Type::Int,
                is_volatile: false,
            },
        )
    }
    fn st(e: Operand, src: Operand) -> Inst {
        inst(
            None,
            InstKind::Store {
                e,
                src,
                is_volatile: false,
            },
        )
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
        Phi {
            dest: v(base, version),
            incoming: vec![(v(base, version - 1), "bb_0".to_string())],
        }
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
        let nodes = vec![
            bin(v("t", 1), vr("a", 1), BinOp::Add, ic(1)),
            bin(v("u", 1), vr("t", 1), BinOp::Add, ic(1)),
        ];
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
        let carried: Vec<(usize, usize, u32)> = g
            .edges
            .iter()
            .filter(|e| e.distance > 0)
            .map(|e| (e.from, e.to, e.distance))
            .collect();
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
        let r = Resources {
            alu: 4,
            ..Default::default()
        };
        assert!(modulo_schedule(&g, 1, &r).is_none(), "II=1 无法满足 4 周期递归环");
        let s = modulo_schedule(&g, 4, &r).expect("II=4 应可行");
        assert!(edges_satisfied(&g, 4, &s));
    }

    #[test]
    fn res_mii_counts_load_slots() {
        let f = func(&["a"]);
        let nodes = vec![
            ld(v("x", 1), vr("a", 1)),
            ld(v("y", 1), vr("a", 2)),
            ld(v("z", 1), vr("a", 3)),
        ];
        let g = graph(&f, &nodes, &[]);
        assert_eq!(
            res_mii(
                &g,
                &Resources {
                    load: 1,
                    ..Default::default()
                }
            ),
            3,
            "3 次 load ÷ 1 槽"
        );
        assert_eq!(
            res_mii(
                &g,
                &Resources {
                    load: 2,
                    ..Default::default()
                }
            ),
            2,
            "2 槽时向上取整"
        );
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

        let nodes2 = vec![
            bin(v("t", 1), vr("x", 1), BinOp::Add, ic(1)),
            ld(v("y", 1), vr("t", 1)),
        ];
        let g2 = graph(&f, &nodes2, &[]);
        assert!(g2.succs(0).any(|e| e.to == 1), "定义 t 必须先于把 t 当地址的装载");
    }

    #[test]
    fn memory_ops_serialize() {
        let f = func(&["a", "t"]);
        let nodes = vec![
            st(vr("a", 1), ic(0)),
            ld(v("t", 1), vr("a", 2)),
            st(vr("a", 1), vr("t", 1)),
        ];
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
        let nodes = vec![
            ld(v("t", 1), vr("x", 1)),
            bin(v("x", 2), vr("t", 1), BinOp::Add, ic(1)),
        ];
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
    /// 「是否还有下一轮」那条判定指令，**不在调度区内**。
    ///
    /// 条件节点不能当普通节点参与流水，两个实测 reason：
    ///   1. 它读的是本轮更新后的归纳变量（即下一轮的值）。一旦把它放进 kernel
    ///      行，那这个读会被解析成轮转 phi（本轮进入值），判定提前一轮 ⇒ 多跑一轮，
    ///      对 `a[i] = i` 就是越界写 `a[trip]`（实测：-O2 输出整块丢失而退出码仍为 0，
    ///      因为越界写踩坏了 stdio 缓冲而不崩溃）。
    ///   2. 把它计入调度区会虚构出并不存在的跳迭代重叠：`red` 原本算出
    ///      `stage=[0,2,0,1,2,1,3]` ⇒ depth=1，其中那个 stage=3 正是条件节点；剔除后
    ///      最大 stage=2 而 II=3 ⇒ depth=0。所以上一轮「已实测到真重叠」的结论
    ///      是假的，必须就此修正。
    pub cond: Inst,
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
    if let Operand::Int(i) = o {
        Some(*i as i64)
    } else {
        None
    }
}

/// `x = v + 0` / `x = 0 + v` 是构建器的**拷贝惯用法**（用它物化一个值），
/// 绝不允许被当成步长 —— 把它计成一步会让真正的 `+1` 冲突、整条递推被误拒。
fn copy_source(k: &InstKind) -> Option<SsaVar> {
    if let InstKind::Bin {
        op: BinOp::Add,
        l,
        r,
    } = k
    {
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
fn iv_step(
    nodes: &[Inst],
    def_of: &HashMap<String, usize>,
    start: &SsaVar,
    iv: &SsaVar,
) -> Option<i64> {
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
        let Some(bb) = func.blocks.get(&name) else {
            continue;
        };
        for inst in &bb.insts {
            let Some(d) = &inst.dest else { continue };
            if d.base != base {
                continue;
            }
            if let InstKind::Bin {
                op: BinOp::Add,
                l,
                r,
            } = &inst.kind
            {
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

/// 本循环是否嵌在其它自然循环之内。
///
/// 为什么现在必须拒嵌套（两条实测错值/挂死，不是理论担心）：
///
/// 1. **外层携带值会被当成循环不变量**。`for(i) for(j) s+=1;` 里 `s` 的 phi 在**外层**
///    头，内层区域读到的 `s.3` 不是内层 phi dest、也不在内层区域内定义 ⇒ 被归为
///    「区域外、原样保留」，于是内层 kernel 每轮都算 `s = 1 + s.3`，累加丢失（实测）。
/// 2. **落地会动到外层赖以前进的块**。往 preheader 追 prologue、往 exit 追 epilogue、
///    摘除 latch、以及「区域外引用改指向最后实例」这一步重写所有非区域块的指令与
///    terminator —— 当这些块位于外层循环体内时，改写的就是外层自己的推进与判定链
///    （实测后果是外层永不退出，产物死循环）。
///
/// 判据只看事实：只要本循环头落在另一个自然循环的体内，就拒。内层流水的收益要等
/// 落地形式做到「不破坏外层结构」时再拿，不能靠先假定安全把死循环送进产物。
/// 本循环是否嵌在**其它**循环体内。
///
/// `doms` 由调用方递下来（`classify_loop` 已经算过一次）：在这里面逐候选头重算
/// 支配集是 O(头数²) 次全函数 COQH 定点，块数上百时编译会从毫秒退到分钟级
/// （实测 255 块的语料上逾 2 分钟）。
fn is_nested_in_other_loop(
    func: &Function,
    header: &str,
    doms: &HashMap<String, HashSet<String>>,
) -> bool {
    for name in crate::ssa::ordered_block_names(func) {
        if name == header {
            continue;
        }
        let Some(bb) = func.blocks.get(&name) else {
            continue;
        };
        if bb.phis.is_empty() {
            continue;
        }
        let mut latches: Vec<String> = pred_names(func, &name)
            .into_iter()
            .filter(|p| dominates(doms, &name, p))
            .collect();
        latches.dedup();
        for lt in latches {
            if crate::ssa::find_loop_body(func, &name, &lt)
                .iter()
                .any(|b| b == header)
            {
                return true;
            }
        }
    }
    false
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
    if is_nested_in_other_loop(func, header, &doms) {
        return Err("本循环嵌在其它循环之内（落地会破坏外层结构，见 is_nested_in_other_loop）");
    }
    if !dominates(&doms, header, header) {
        return Err("头块不可达");
    }

    // latch = 被头支配的前驱，必须唯一。
    let mut latches: Vec<String> = pred_names(func, header)
        .into_iter()
        .filter(|p| dominates(&doms, header, p))
        .collect();
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
    let body: HashSet<String> = crate::ssa::find_loop_body(func, header, &latch)
        .into_iter()
        .collect();
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
    let outside: Vec<String> = pred_names(func, header)
        .into_iter()
        .filter(|p| *p != latch)
        .collect();
    if outside.len() != 1 {
        return Err(if outside.is_empty() {
            "头没有区域外前驱（不可达循环）"
        } else {
            "多个区域外前驱（循环被多路进入）"
        });
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
    if nodes.iter().any(|i| !inst_is_materializable(&i.kind)) {
        return Err("调度区内有不可按迭代复制的指令（异常边/用户声明/asm 输出）");
    }
    if nodes.len() > 64 {
        return Err("调度区超过 64 条指令（方案的准入上限）");
    }
    // 判定指令不进调度区（为什么必须剔除，见 `PipelinedLoop::cond` 的注释）。
    // 它的位置随循环形态不同：`for` 在 latch（`for.step` 算下一轮的 `i<8`），
    // `while` 在头块自己（`while.cond` 带分支）。两处都要看，否则 `while` 形态会被
    // 误拒为「latch 不是条件分支」。
    let cond_name = branch_cond_var(&func.blocks[&latch].term)
        .or_else(|| branch_cond_var(&func.blocks[header].term))
        .ok_or("循环判定不是变量比较")?;
    // 条件的生产者必须就在调度区里，否则 kernel 无法自己判定是否继续。
    let cond_at = match nodes
        .iter()
        .position(|i| i.dest.as_ref().map(|d| d.name()) == Some(cond_name.clone()))
    {
        Some(p) => p,
        None => return Err("循环条件由调度区外计算，kernel 无法自洽判定"),
    };
    let cond = nodes.remove(cond_at);
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
    let (bound_var, op, limit, var_left) =
        match (loop_bound(func, header), loop_bound(func, &preheader)) {
            (Some(a), None) => a,
            (None, Some(a)) => a,
            (Some(_), Some(_)) => {
                return Err("头块与 preheader 各有一个入环比较（不猜哪个控制回边）")
            },
            (None, None) => return Err("入环比较不是「变量 vs 常量」，trip 未知"),
        };
    let op = if var_left { op } else { flip_cmp(op) };
    let iv_base = bound_var.base.clone();
    let init = const_def_outside(func, &region_bbs, &iv_base)
        .ok_or("归纳变量初值不是区域外的唯一常量定义")?;
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
        .and_then(|p| {
            p.incoming
                .iter()
                .find(|(_, from)| *from == latch)
                .map(|(s, _)| s.clone())
        })
        .ok_or("归纳变量没有 latch 侧 phi")?;
    let step = iv_step(&nodes, &def_of, &start, &phi_dest)
        .ok_or("归纳变量不是「沿拷贝链恰好一步」的线性递推")?;
    if step <= 0 {
        return Err("步长非正（本层只旋转上升的归纳变量）");
    }
    let span = match op {
        BinOp::Lt => limit.checked_sub(init).ok_or("trip 计算溢出")?,
        BinOp::Le => limit
            .checked_sub(init)
            .and_then(|d| d.checked_add(1))
            .ok_or("trip 计算溢出")?,
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
        cond,
        trip: trip as usize,
    })
}

fn bb_phis_empty(func: &Function, name: &str) -> bool {
    func.blocks
        .get(name)
        .map(|b| b.phis.is_empty())
        .unwrap_or(false)
}

/// 该指令能否被**按迭代复制**成多份实例。
///
/// 不是所有指令复制多份都还等价：
///   * `Invoke` 复制会把一个受保护调用点撑成 N 个 call-site 区间，落地垫的
///     `%rax/%rdx` 交接也会错位；`EhLandingPad` 必须是其块的第一条指令。
///   * `PreservedDecl` 是**用户声明**，复制多份就是重复定义；
///   * `InlineAsm` 的 `output_vars` 是写侧名字，多实例共用同一个名字会互相覆盖。
/// 物化层只重写读操作数与 dest，处理不了这些形态，所以在准入处就拒掉。
fn inst_is_materializable(k: &InstKind) -> bool {
    !matches!(
        k,
        InstKind::Invoke { .. }
            | InstKind::EhLandingPad { .. }
            | InstKind::PreservedDecl { .. }
            | InstKind::InlineAsm { .. }
    )
}

fn branch_cond_var(term: &crate::ssa::Terminator) -> Option<String> {
    if let crate::ssa::Terminator::Branch {
        cond: Operand::Var(v),
        ..
    } = term
    {
        Some(v.name())
    } else {
        None
    }
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
    const UNKNOWN: &str =
        "int unknown(int n,int*a){int s=0;for(int i=0;i<n;i++)s+=a[i];return s;}\n";
    const NEST: &str =
        "int nest(int n){int s=0;for(int i=0;i<4;i++)for(int j=0;j<4;j++)s+=1;return s;}\n";

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
        // 调度区不含判定指令：`for.body` 2 条 + `for.step` 5 条 - 1 条条件 = 6。
        assert_eq!(l.nodes.len(), 6, "区域拼接序 7 条减去判定指令");
        assert_eq!(
            l.cond.dest.as_ref().map(|d| d.name()),
            Some("_tbin_6".to_string()),
            "判定指令必须就是 latch 分支所读的那条"
        );
        assert_eq!(l.iv_base, "i");
        assert_eq!(l.trip, 8);
        // 每个 phi 的 latch 侧值都必须在区域内产生，且 phi_sources 指向它。
        for p in &f.blocks["for.body"].phis {
            let idx = *l
                .phi_sources
                .get(&p.dest.name())
                .unwrap_or_else(|| panic!("phi {} 没有 phi_sources", p.dest.name()));
            let d = l.nodes[idx].dest.as_ref().unwrap();
            let want = p
                .incoming
                .iter()
                .find(|(_, from)| *from == l.latch)
                .unwrap()
                .0
                .clone();
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
        assert!(
            errs.iter()
                .any(|(_, e)| *e == "步长非正（本层只旋转上升的归纳变量）"),
            "{errs:?}"
        );
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
        assert!(
            errs.iter()
                .any(|(_, e)| *e == "下降比较（本层只处理上升归纳变量）"),
            "{errs:?}"
        );

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

    /// 嵌套循环的**两层都必须被拒**：外层因为区域不是直线链（内层块带 phi），
    /// 内层因为「本循环嵌在其它循环之内」。第二条判据不是过度保守：拿掉它之后
    /// 实测产物出现两类错（外层携带值被当不变量、外层推进链被重写至死循环），
    /// 详见 `is_nested_in_other_loop` 与 `tests/fixtures/opt/ssa_pipe_nested.c`。
    #[test]
    fn nested_loops_are_refused_on_both_levels() {
        let f = fn_ssa(NEST, "nest");
        let r = rejections(&f, &Resources::default());
        assert_eq!(r.len(), 2, "两个头都必须被拒：{r:?}");
        assert!(
            r.iter().any(|(_, e)| e.contains("嵌在其它循环之内")),
            "内层需给出嵌套原因：{r:?}"
        );
        assert!(
            r.iter()
                .any(|(_, e)| e.contains("直线链") || e.contains("带 phi")),
            "外层需给出形状原因：{r:?}"
        );
        assert!(plans_for(&f, &Resources::default()).is_empty(), "嵌套场景不得产出任何计划");
    }

    /// 实测：load 归约 **没有**跳迭代重叠，闸门必须拒它。
    ///
    /// 上一轮这里断言的是「depth=1 真重叠」，那是假的：当时把 latch 的判定指令
    /// 也算进了调度区，它被排到 stage=3 而撑出行跳。判定指令本身不能进调度区
    /// （见 `PipelinedLoop::cond` 两条理由），剔除后最大 stage=2 而 II=3 ⇒ depth=0。
    /// 流水一个无重叠的循环只是搬指令，所以拒收是正确行为。
    #[test]
    fn load_reduction_has_no_cross_iteration_overlap() {
        let f = fn_ssa(RED, "red");
        let lp = classify_loop(&f, "for.body").expect("red 应被识别");
        let g = build_dep_graph(&lp.nodes, &f, &f.blocks["for.body"], &lp.phi_sources);
        let r = Resources::default();
        assert_eq!(rec_mii(&g), 3, "闭合归约环的 RecMII 仍为 3（load 延迟 2 + 加 1）");
        let ii = rec_mii(&g).max(res_mii(&g, &r));
        let st = modulo_schedule(&g, ii, &r).expect("MII 处必须可行");
        assert!(edges_satisfied(&g, ii, &st));
        assert_eq!(st.iter().copied().max().unwrap_or(0) / ii, 0, "剔除判定指令后不应有跳行重叠");
        let e = plan_for_header(&f, "for.body").expect_err("无重叠就不该流水");
        assert!(e.contains("无跳迭代重叠"), "实得：{e}");
    }

    /// 真值得流水的形态：同一迭代内串起来的长依赖链。II 由资源决定（每节点只 1
    /// 延迟的链不抬 RecMII），而链长把 stage 拉开到多行 ⇒ depth ≥ 1。
    #[test]
    fn dependency_chain_yields_real_overlap_and_is_accepted() {
        let src = "int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}\n";
        let f = fn_ssa(src, "ch");
        let p =
            plan_for_header(&f, "for.body").unwrap_or_else(|e| panic!("依赖链循环应过闸门：{e}"));
        assert!(p.inst.depth >= 1, "依赖链必须给出跳迭代重叠：ii={} stage={:?}", p.ii, p.stage);
        assert_eq!(p.inst.depth, 1, "本层只做一级轮转，接受的计划 depth 必须恰为 1");
        assert_eq!(p.inst.kernel.len() + p.inst.depth, p.lp.trip);
        assert_eq!(
            p.inst.flatten().len(),
            p.lp.nodes.len() * p.lp.trip,
            "实例覆盖 = 节点数 × 迭代数"
        );
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
    /// **真轮转格**（P12-B 的「版本粒度的家」在流水侧的消费）：空表就是旧的
    /// 基名塌缩模型（一级轮转 + 行内反依赖约束）；非空时这些跳轮读不再读生产者
    /// 自己的格子，而是读 `home` 那个**独立局部**（自己的家：一个寄存器或帧内带
    /// 里的一个槽）。名字由 `crate::ssa::location::LocMap` 的共享占名分配器给出，
    /// 所以与版本分裂不会撞上。
    pub cells: Vec<RotCell>,
}

/// 一个轮转格：生产者节点 `q` 往回 `lag` 轮（`lag ≥ 1`）的那份值住在哪个基名里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotCell {
    /// 生产者节点号。
    pub q: usize,
    /// 跳几轮（`back_rounds` 的结果）。
    pub lag: i64,
    /// 该格的基名（形如 `a_s1`）：它 own 一个 AST 局部，因此 own 一个家。
    pub home: String,
    /// 播种要读的那个**携带 phi 名**（仅当该格的初值是「进循环前的那份值」时需要）。
    /// `Some` 意味着两个不同的携带 phi 共享同一格就是两个可能不同的初值 ⇒ 必须拒；
    /// `None` 意味着初值就是某个已物化的实例，谁读都同一份 ⇒ 多个名字共格合法。
    pub seed: Option<String>,
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

    /// 行内位置表（节点 → 行内下标），由 kernel 第一行给出。
    ///
    /// 行内序**是**语义的一部分：SSA→AST 重建把同基名的 `e.5` / `e.14` 还原成同一个
    /// 变量（同一个槽），所以「谁先发射」直接决定读者拿到哪一份值。行与行必须一致，
    /// 否则轮与轮之间就没有确定的语义可言 —— 这里宁可直接判 None 也不猜。
    fn row_pos(&self) -> Option<Vec<usize>> {
        let n = self.shift.len();
        let row0 = self.kernel.first()?;
        if row0.len() != n || self.kernel.iter().any(|r| r != row0) {
            return None;
        }
        let mut pos = vec![usize::MAX; n];
        for (i, &p) in row0.iter().enumerate() {
            if p >= n || pos[p] != usize::MAX {
                return None;
            }
            pos[p] = i;
        }
        Some(pos)
    }

    /// kernel 行（所有行相同，见 `row_pos`）。
    pub fn row(&self) -> Option<&Vec<usize>> {
        if self.row_pos().is_some() {
            self.kernel.first()
        } else {
            None
        }
    }

    /// 绝对时间序（平局尺子：先 T，再**行内位置**，再节点号）。
    ///
    /// 平局用行内位置而不是 `stage%ii`：平局意味着两者 T 相同、图上无约束，
    /// 于是发射序就是唯一真源（见 `row_pos`）。拿不到合法行序时给一个不可能
    /// 出现的下标，让后面的逐元素相等比较自然失败，而不是另起一套规则。
    fn key(&self, stage: &[usize], pos: &[usize], x: &(usize, usize)) -> (usize, usize, usize) {
        (stage[x.0] + x.1 * self.ii, pos.get(x.0).copied().unwrap_or(usize::MAX), x.0)
    }

    /// 换一份行内序：所有 kernel 行同步替换，prologue/epilogue 按新平局尺子归位。
    ///
    /// 只接受「同一节点集合的重排」，并且换完必须还能 `verify`，所以调用方拿到的
    /// 计划要么与新行序自洽、要么被拒 —— 不存在「行序换了但切分还是旧的」的中间态。
    pub fn with_row(&self, stage: &[usize], row: &[usize]) -> Result<InstancePlan, &'static str> {
        let n = self.shift.len();
        let mut sorted = row.to_vec();
        sorted.sort_unstable();
        if sorted.len() != n || sorted.iter().enumerate().any(|(i, &p)| i != p) {
            return Err("新行序不是节点集的一个排列");
        }
        let mut pos = vec![0usize; n];
        for (i, &p) in row.iter().enumerate() {
            pos[p] = i;
        }
        let mut out = self.clone();
        out.kernel = vec![row.to_vec(); self.kernel.len()];
        let tkey = |x: &(usize, usize)| self.key(stage, &pos, x);
        out.prologue = self.prologue.clone();
        out.epilogue = self.epilogue.clone();
        out.prologue.sort_by_key(tkey);
        out.epilogue.sort_by_key(tkey);
        if !out.verify(stage) {
            return Err("重排后的行序与实例覆盖/T 序不再自洽");
        }
        Ok(out)
    }

    /// 每个节点被实际执行的次数（发射序视角）。合法切分里必须恰为 trip 次。
    pub fn exec_counts(&self) -> Vec<usize> {
        let mut c = vec![0usize; self.shift.len()];
        for &(p, _) in &self.flatten() {
            if p < c.len() {
                c[p] += 1;
            }
        }
        c
    }

    /// 证明这份切分是合法的：**每个 (节点, 迭代) 恰好执行一次**，且展开序与 T 序
    /// 逐元素相等。
    ///
    /// 这条自检不是装饰。上一版实现里它当场抓出两个方向性错误：kernel 行把迭代号
    /// 一同复制（每轮跑同一批迭代）、跳轮引用偏移符号写反（所有真实循环被误拒）。
    /// 两者都不会让编译失败，只会让产物错值。
    pub fn verify(&self, stage: &[usize]) -> bool {
        let n = self.shift.len();
        let pos = match self.row_pos() {
            Some(p) => p,
            None => return false,
        };
        if stage.len() != n {
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
        let mut want: Vec<(usize, usize)> = (0..n)
            .flat_map(|p| (0..trip).map(move |k| (p, k)))
            .collect();
        want.sort_by_key(|x| self.key(stage, &pos, x));
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
    let kernel = vec![row.clone(); rounds];
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
    // 两段各自按绝对时间归位（平局尺子就是行内位置，与 `verify` 用的是同一把）。
    // 不排就凑不出 T 序，下面的逐元素相等会直接拒收 —— 这正是自检该做的事。
    let mut pos = vec![0usize; n];
    for (i, &q) in row.iter().enumerate() {
        pos[q] = i;
    }
    let tkey = |x: &(usize, usize)| (stage[x.0] + x.1 * ii, pos[x.0], x.0);
    prologue.sort_by_key(tkey);
    epilogue.sort_by_key(tkey);
    let plan = InstancePlan {
        ii,
        depth,
        shift,
        kernel,
        prologue,
        epilogue,
        cells: Vec::new(),
    };
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
        assert_eq!(p.flatten(), vec![(1, 0), (0, 0), (1, 1), (0, 1), (1, 2), (0, 2)]);
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
            let crate::ast::Decl::Func(f) = decl else {
                continue;
            };
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
            assert_eq!(
                plan.flatten().len(),
                l.nodes.len() * l.trip,
                "实例覆盖数 = 节点数 × 迭代数"
            );
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

/// 真轮转的上限：**轮转级数**（最多同时保留几份旧值）与**格子总数**（每轮行尾要
/// 发多少条拷贝）。这两条不是「不敢做」，而是代价判据：级数 N 时 prologue/epilogue
/// 各自多 N 轮、每轮多 O(格子数) 条拷贝，而收益仍是同一个 II。上限外的循环
/// 宁可继续不收（与旧行为的「一律拒」相比，这里只拒代价不划算的那部分）。
pub const MAX_ROT_DEPTH: usize = 8;
/// 每轮行尾的拷贝链上限（跳迭代读的对数）。
pub const MAX_ROT_CELLS: usize = 16;

/// 单次流水允许的实例总数上限（= 循环体节点数 × 静态 trip）。
///
/// 本层把三段切成 `节点数 × trip` 个实例（`instance_plan` 里的
/// `for k in 0..trip` 逐趟枚举），编译时间/内存随 trip 线性增长。对
/// trip 极大的静态计数循环（如 `for (i = 0; i < 50000000; i++)`）会把
/// 编译器拖入分钟级乃至挂起，而软件流水本就只对 trip 适中的循环划算。
/// 超上限在建依赖图之前直接放弃（退回普通循环，语义不变）。上限取得足够
/// 高：任何能成功过闸的循环（否则早已挂起/膨胀）实例数都远低于它，因此
/// 不改变既有产物。
pub const MAX_PIPE_INSTANCES: usize = 1 << 20;

/// 轮转路径的收益判据：前两条与塌缩模型完全一致（这样「depth=0 / trip 太短」的拒因
/// 文案在两模型下相同），只把 `depth > 1` 换成上限判据。
pub fn benefit_verdict_rot(
    depth: usize,
    ii: usize,
    trip: usize,
    cells: usize,
) -> Result<(), &'static str> {
    if depth == 0 {
        return Err("无跳迭代重叠 (depth=0)：流水只是搬动指令");
    }
    if trip < 2 * ii {
        return Err("trip 太短：prologue/epilogue 开销盖过收益");
    }
    if depth > MAX_ROT_DEPTH {
        return Err("轮转级数超上限：prologue/epilogue 代价盖过收益");
    }
    if cells > MAX_ROT_CELLS {
        return Err("轮转格数超上限：每轮行尾的拷贝链太长");
    }
    Ok(())
}

/// 轮转格的值类型是否能用一条 `Cast`（同型转换 = 原样拷贝）拿到自己的格子。
/// 复数/聚合/数组/向量/_Atomic 不行：它们的重建形状不是「一个标量局部」，
/// 给它们造格子就是拿未验证的形状去换收益（宁可不接受该循环）。
fn rot_cell_ty_supported(ty: &crate::ast::Type) -> bool {
    use crate::ast::Type as T;
    !matches!(
        ty,
        T::Complex(_)
            | T::Imaginary(_)
            | T::Array(..)
            | T::Struct(..)
            | T::Union(..)
            | T::Vector(..)
            | T::Atomic(_)
            | T::Func(..)
            | T::Void
    )
}

// ========================================= 发射序：基名塌缩的反依赖与判定
//
// SSA→AST 重建把 `e.4` / `e.5` / `e.14` 全都还原成同一个变量 `e`（同一个栈槽），
// head 块上那对轮转 phi 因此折叠成 `e = e` 被丢掉。于是「读上一轮的 e」在机器码里
// 并不是读一个轮转寄存器，而是**读那个槽的当前内容** —— 它等于哪一份实例完全由
// 发射顺序决定：消费者必须排在生产者本轮写之前，才拿得到上一轮的值。
//
// 模调度的 T 序模型表达不了这件事：它只给真数据依赖定序，而这里是**反依赖**（WAR）。
// 实测 chain6_10（6 级依赖链、trip=10）就是这么错的：kernel 行把 `f += e` 排在
// `e += d` 之后 ⇒ f 每轮读到本轮的 e，少加第一轮、多算最后一轮，-O2 得 7006（真值 5005）。
//
// 两道判据：
//   * `refine_kernel_row`：按行内约束（本轮前向 = 生产者先，轮转读 = 消费者先）稳定
//     重排平局，不改变任何实例的绝对时间；
//   * `emitted_reads_match`：沿**实际发射序**（prologue → 每轮「head 判定 + kernel 行」
//     → epilogue）模拟槽位内容，要求每个读都命中模型指定的那个实例。
// 后者是发射层的最终闸门：漏掉任何一条边，它以「读到的不是预期实例」的形式炸出来，
// 而不是以错值进产物。前者只是让后者能通过的重排，不参与正确性论证。

/// 消费者（轮偏移 `shift_p`）读生产者 q 的输出要往回几轮。唯一真源：行内约束、
/// 发射序模拟与落地三处共用它，两处各写一套正是上一版「测试绿而实际错」的根源。
/// 判定指令不占节点号，它的 `shift_p` 就是 `cond_shift`。
fn back_rounds(shift_p: usize, shift_q: usize, carried: bool) -> i64 {
    shift_q as i64 - shift_p as i64 + if carried { 1 } else { 0 }
}

/// 判定指令在发射序里的轮偏移：它在 khead 块、kernel 行**之前**执行。
///
/// 为什么是 `depth - 1` 而不是常量 0（旧代码写死 0，而 `depth ≤ 1` 时两者相同，
/// 所以塌缩模型从来没暴露过）。设 kernel 第 r 行的判定读到的是原程序「第 k(r) 轮
/// 循环尾的那次判定」。原判定在第 k 轮尾为真当且仅当还有一轮要跑，所以：
///   * kernel 必须恰好跑 `rounds = trip - depth` 行（`prologue + rows + epilogue`
///     的实例覆盖已由 `verify` 钉死：每节点恰 `trip` 次）；
///   * 于是「判定为真」必须等价于 `r ≤ rounds - 1`；
///   * 而原判定读的是第 k 轮产的归纳变量值（本形状里 `i_new = k + 1`，与 `trip` 比），
///     它为真当且仅当 `k ≤ trip - 2`。
/// 两式对齐 ⇒ `k(r) = r + depth - 1`，即把判定看成轮偏移为 `depth - 1` 的节点。
/// 实测：写回 0 时 `rb`（depth=2、trip=32、rounds=30）会多跑一整行（第 30 行仍为真），
/// 而那一行的累加器实例在 epilogue 里又跑一遗 ⇒ 375 变 391（恰好多一个 `d` 值 16）。
fn cond_shift(depth: usize) -> usize {
    depth.saturating_sub(1)
}

/// 行内约束 `(u, v)`：u 必须排在 v 之前。
///
/// 两类约束，第二类只在**没有轮转格**时存在：
///   * 本轮前向（`back == 0`）：这是图上的真数据依赖（距离 0），生产者必须先发射；
///   * 跳轮读（`back ≥ 1`）：基名塌缩模型下它读的是同一个槽，所以消费者必须排在
///     生产者本轮写**之前**（WAR）；一旦该 (生产者, lag) 有自己的格子，这次读就不再
///     碰生产者的槽 ⇒ 约束消失。这就是「同基名无法同时保留两份 ⇒ 保守拒收」被
///     解除的那一处：行内约束不再成环，平局也就不会被强行重排。
fn row_constraints(lp: &PipelinedLoop, plan: &InstancePlan, row: &[usize]) -> Vec<(usize, usize)> {
    let mut def_of: HashMap<String, usize> = HashMap::new();
    for (i, inst) in lp.nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            def_of.insert(d.name(), i);
        }
    }
    let shift = &plan.shift;
    let mut out = Vec::new();
    for &p in row {
        for v in kind_uses(&lp.nodes[p].kind) {
            let key = v.name();
            let (q, carried) = if let Some(&q) = lp.phi_sources.get(&key) {
                (q, true)
            } else if let Some(&q) = def_of.get(&key) {
                (q, false)
            } else {
                continue;
            };
            let back = back_rounds(shift[p], shift[q], carried);
            if back == 0 {
                // 本轮前向：生产者必须先发射。
                out.push((q, p));
            } else if back >= 1 && p != q && !has_cell(&plan.cells, q, back) {
                // 轮转（要上一轮的值）而没有自己的格子：消费者必须在生产者本轮写
                // 之前读。累加器自读自己（p==q）由「上一轮已写入槽」天然满足。
                out.push((p, q));
            }
        }
    }
    out
}

/// 该 (生产者, lag) 是否已有轮转格（行内约束与发射序模拟共用的唯一查法）。
fn has_cell(cells: &[RotCell], q: usize, lag: i64) -> bool {
    cells.iter().any(|c| c.q == q && c.lag == lag)
}

/// 这份切分需要哪些轮转格：所有「跳 ≥ 1 轮」的读，按 (生产者节点, lag) 归并。
///
/// 为什么在闸门里算而不是在落地时算：行内约束与发射序模拟必须读**同一份**格子表，
/// 否则一边按「有格子」放行、另一边按「同一个槽」校验，判据就成了两套真源。
/// 名字也在这里定下来（`LocMap::reserve_split_name`，与版本分裂同一个分配器），
/// 落地只照抄，不再分配一次 —— 于是「同一个基名的 K 份版本各自有位置」这件事在
/// 计划里就是可观测的，而不是等到 AST 才发现名字撞了。
pub fn needed_cells(
    func: &Function,
    lp: &PipelinedLoop,
    plan: &InstancePlan,
) -> Result<Vec<RotCell>, &'static str> {
    let row = match plan.row() {
        Some(r) => r.clone(),
        None => return Err("kernel 行不合法（无法算轮转格）"),
    };
    let shift = &plan.shift;
    let mut def_of: HashMap<String, usize> = HashMap::new();
    for (i, inst) in lp.nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            def_of.insert(d.name(), i);
        }
    }
    let mut keys: std::collections::BTreeMap<(usize, i64), Option<String>> =
        std::collections::BTreeMap::new();
    let cs = cond_shift(plan.depth);
    let mut note =
        |p_shift: usize, q: usize, carried: bool, key: String| -> Result<(), &'static str> {
            let back = back_rounds(p_shift, shift[q], carried);
            if back < 1 {
                return Ok(()); // 本轮前向（靠行内序）或区域外引用：不需要格子
            }
            let d = lp.nodes[q].dest.as_ref().ok_or("轮转生产者没有输出")?;
            if !rot_cell_ty_supported(&d.ty) {
                return Err("轮转值的类型不是可拷贝标量（聚合/数组/向量/复数/_Atomic）");
            }
            // 格子的初值就是实例 `(q, shift[q] - back)`，而 `back_rounds` 展开后它恰等于
            // `p_shift - (携带?1:0)`（只与读者有关，与生产者无关）。它为负意味着「要的
            // 是进循环前的那份值」，于是这一格必须能从**一个确定的 phi** 拿到初值；
            // 两个不同的携带 phi 共享同一格就是两个可能不同的初值 ⇒ 拒。初值不是
            // 「进循环前」那一档时没有这个约束：同一个 (生产者, lag) 本来就是同一个实例，
            // 被两个名字读完全等价（实测 `rb` 里判定读 `i.2`、体内读 `i.3` 就是同一份值的
            // 两个名字，把它们拆成两格既错也白）。
            let want = p_shift as i64 - if carried { 1 } else { 0 };
            let seed = if want < 0 { Some(key) } else { None };
            match keys.get(&(q, back)) {
                None => {
                    keys.insert((q, back), seed);
                },
                Some(prev) => {
                    if *prev != seed {
                        return Err("同一轮转格被两个不同的携带名字读（初值可能不同）");
                    }
                },
            }
            Ok(())
        };
    for &p in &row {
        for v in kind_uses(&lp.nodes[p].kind) {
            let key = v.name();
            if let Some(&q) = lp.phi_sources.get(&key) {
                note(shift[p], q, true, key)?;
            } else if let Some(&q) = def_of.get(&key) {
                note(shift[p], q, false, key)?;
            }
        }
    }
    // 判定指令也读跳轮的值：它在 khead 里、body 之前执行，轮偏移是 `cond_shift`。
    for v in kind_uses(&lp.cond.kind) {
        let key = v.name();
        if let Some(&q) = lp.phi_sources.get(&key) {
            note(cs, q, true, key)?;
        } else if let Some(&q) = def_of.get(&key) {
            note(cs, q, false, key)?;
        }
    }
    // 占名名单与分配器都只有一份：`LocMap::reserved_names` （原始局部 + 形参 + IR 里
    // 出现过的全部基名）+ `reserve_split_name`。按 (节点, lag) 升序分配 ⇒ 确定序。
    let mut used = crate::ssa::location::LocMap::reserved_names(func);
    let mut out: Vec<RotCell> = Vec::new();
    for (&(q, lag), seed) in &keys {
        let base = lp.nodes[q]
            .dest
            .as_ref()
            .ok_or("轮转生产者没有输出")?
            .base
            .clone();
        let home = crate::ssa::location::LocMap::reserve_split_name(&mut used, &base, 1);
        out.push(RotCell {
            q,
            lag,
            home,
            seed: seed.clone(),
        });
    }
    Ok(out)
}

/// 按行内约束重排 kernel 行。稳定排序（无约束的保持原相对序），所以能不动就不动；
/// 约束成环说明基名塌缩下拿不到两全的序（要真轮转寄存器才行）⇒ 拒，并给出原因。
pub fn refine_kernel_row(
    lp: &PipelinedLoop,
    plan: &InstancePlan,
    stage: &[usize],
) -> Result<InstancePlan, &'static str> {
    let row0 = match plan.row() {
        Some(r) => r.clone(),
        None => return Err("kernel 行不合法（行间不一致或有重/漏节点）"),
    };
    let n = row0.len();
    let mut rank = vec![0usize; n];
    for (i, &p) in row0.iter().enumerate() {
        rank[p] = i;
    }
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for (u, v) in row_constraints(lp, plan, &row0) {
        if u >= n || v >= n || u == v || !seen.insert((u, v)) {
            continue;
        }
        succ[u].push(v);
        indeg[v] += 1;
    }
    // Kahn：每步在所有入度 0 的节点里选原行序最靠前的 ⇒ 结果确定，且与旧实现的
    // 「按 stage%ii 排」在无约束时逐字相同（产物字节不因此抖动）。
    let mut out: Vec<usize> = Vec::with_capacity(n);
    let mut done = vec![false; n];
    while out.len() < n {
        let pick = (0..n)
            .filter(|&x| !done[x] && indeg[x] == 0)
            .min_by_key(|&x| rank[x])
            .ok_or("行内反依赖约束成环：基名塌缩下无法两全（需真轮转寄存器）")?;
        done[pick] = true;
        out.push(pick);
        for &s in &succ[pick] {
            if !done[s] {
                indeg[s] -= 1;
            }
        }
    }
    if out == row0 {
        return Ok(plan.clone());
    }
    plan.with_row(stage, &out)
}

/// 一次读落在**哪个格子**上，以及格子此刻应该装哪份实例。
///
/// 返回 `(存储键, 期望内容)`。存储键在两个模型下不同：
///   * 基名塌缩模型（`cells` 为空，或该跳轮读没格子）：`v.base` —— 同基名只有一个槽；
///   * 真轮转模型：跳 ≥ 1 轮的读落在该 (生产者, lag) 的格子 `home` 上。
/// `cons_shift` 为 `Some(s)` 表示这次读发生在 kernel 的某一行里（消耗者的轮偏移 s，
/// 判定指令是 `cond_shift(depth)`）；`None` 表示 prologue/epilogue 的实例——那里落地时
/// 直接按实例名字引用，不经过轮转格，所以上面仍是基名级的槽。
fn sim_read_target(
    lp: &PipelinedLoop,
    def_of: &HashMap<String, usize>,
    shift: &[usize],
    cells: &[RotCell],
    v: &SsaVar,
    k: usize,
    cons_shift: Option<usize>,
) -> (String, Option<(usize, usize)>) {
    let key = v.name();
    // 模型要的是哪个实例：携带值 = 上一迭代，区域内定义 = 同迭代，
    // 区域外（参数/循环前的定义）= 进循环前的值（槽还没被区域内的写盖住）。
    let mut want: Option<(usize, usize)> = None;
    let mut prod: Option<usize> = None;
    if let Some(&q) = lp.phi_sources.get(&key) {
        prod = Some(q);
        want = if k == 0 { None } else { Some((q, k - 1)) };
    } else if let Some(&q) = def_of.get(&key) {
        prod = Some(q);
        want = Some((q, k));
    }
    let mut store = v.base.clone();
    if let (Some(s), Some(q)) = (cons_shift, prod) {
        // 轮数差 = 消费者所在轮 (k - s) 与生产者所在轮 (wk - shift[q]) 之差。
        // 与 `back_rounds` 同一个公式：携带时 wk = k-1 会得 lag = shift[q]-s+1，
        // 非携带时 wk = k 会得 lag = shift[q]-s —— 两处各自写一套就是上一版「测试绿
        // 而实际错」的根源，所以这里只允许从 lag 反查格子。
        if let Some((_, wk)) = want {
            let lag = (k as i64 - s as i64) - wk as i64 + shift[q] as i64;
            if lag >= 1 {
                if let Some(c) = cells.iter().find(|c| c.q == q && c.lag == lag) {
                    store = c.home.clone();
                }
            }
        }
    }
    (store, want)
}

/// 模拟一次读：检查格子此刻的内容正是模型指定的那份实例。
fn sim_check_read(
    lp: &PipelinedLoop,
    def_of: &HashMap<String, usize>,
    shift: &[usize],
    cells: &[RotCell],
    v: &SsaVar,
    k: usize,
    cons_shift: Option<usize>,
    slot: &HashMap<String, Option<(usize, usize)>>,
) -> Result<(), &'static str> {
    let (store, want) = sim_read_target(lp, def_of, shift, cells, v, k, cons_shift);
    let got = slot.get(&store).copied().unwrap_or(None);
    if got == want {
        Ok(())
    } else if store != v.base {
        Err("轮转格里的值不是模型指定的实例（拷贝链与跳轮读不一致）")
    } else {
        Err("发射序下该读拿到的不是模型指定的实例（基名塌缩反依赖）")
    }
}

/// 沿实际发射序模拟格子语义：`cells` 为空时就是「每个基名一个槽」的塌缩模型；
/// 非空时额外维护轮转格，并在每轮行尾做一次链式后移（`L` 从大到小）。
/// 任何一处读错实例都直接拒收。
pub fn emitted_reads_match(lp: &PipelinedLoop, plan: &InstancePlan) -> Result<(), &'static str> {
    let n = lp.nodes.len();
    let row = match plan.row() {
        Some(r) => r.clone(),
        None => return Err("kernel 行不合法（无法模拟发射序）"),
    };
    let mut def_of: HashMap<String, usize> = HashMap::new();
    let mut base_of: Vec<Option<String>> = vec![None; n];
    for (i, inst) in lp.nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            def_of.insert(d.name(), i);
            base_of[i] = Some(d.base.clone());
        }
    }
    let rounds = plan.kernel.len();
    let trip = rounds + plan.depth;
    let cells = &plan.cells;
    // 判定指令的轮偏移与闸门、落地同一个公式（不是常量）：depth ≥ 2 时它少跳 depth-1 轮。
    let cs = cond_shift(plan.depth);
    let mut slot: HashMap<String, Option<(usize, usize)>> = HashMap::new();
    // 播种：与落地**同一个发射位置**（`want = shift[q] - lag < 0` ⇒ prologue 之前；
    // 否则 ⇒ prologue 实例 `(q, want)` 之后）。这里必须真的**读基名槽**，而不是按
    // 公式写死格子内容：按公式写就看不见「锚点此刻槽里其实是同基名的另一次写」这类
    // 错值，而落地（`a_s1 = a`）恰恰会拿到那一份。负数下标对应「进循环前的值」，
    // 与携带值在 `k == 0` 时的读法同一个口径（模型里就是 `None`）。
    let want_of = |c: &RotCell| -> i64 { plan.shift[c.q] as i64 - c.lag };
    let seed_one = |slot: &mut HashMap<String, Option<(usize, usize)>>,
                    c: &RotCell|
     -> Result<(), &'static str> {
        let want = plan.shift[c.q] as i64 - c.lag;
        let Some(b) = base_of[c.q].clone() else {
            return Err("轮转生产者没有输出");
        };
        let cur = slot.get(&b).copied().unwrap_or(None);
        let expect: Option<(usize, usize)> =
            if want < 0 { None } else { Some((c.q, want as usize)) };
        if cur != expect {
            return Err("轮转格的播种点拿不到预期实例（基名槽在锚点已被同基名的另一次写盖住）");
        }
        slot.insert(c.home.clone(), cur);
        Ok(())
    };
    // 行尾的拷贝链：与落地**同序**（同一基名内 lag 从大到小），否则模拟与机器码
    // 两套语义：`a_s3 = a_s2; a_s2 = a_s1; a_s1 = a;` 是自下而上的链，先写小 lag 就会
    // 把本轮的新值传下去（每一格都拿到同一个最新值），链就白轮了。
    let shift_cells = |slot: &mut HashMap<String, Option<(usize, usize)>>,
                       r: usize|
     -> Result<(), &'static str> {
        let mut order: Vec<usize> = (0..cells.len()).collect();
        order.sort_by(|&x, &y| {
            let (a, b) = (&cells[x], &cells[y]);
            a.q.cmp(&b.q).then(b.lag.cmp(&a.lag))
        });
        for &i in &order {
            let c = &cells[i];
            let src = if c.lag == 1 {
                // 一级格直接拿生产者本轮的输出：它必须仍住在基名槽里（行尾没人
                // 再写同一个基名），否则 `a_s1 = a` 拿到的是别的写。这条不是走过场：
                // 一个迭代里同一基名被写两次时，旧模型靠行内序护住，而拷贝链只能
                // 在行尾拿 ⇒ 必须点名拒收（而不是静默拿错）。
                let Some(b) = base_of[c.q].clone() else {
                    return Err("轮转生产者没有输出");
                };
                let cur = slot.get(&b).copied().unwrap_or(None);
                if cur != Some((c.q, r + plan.shift[c.q])) {
                    return Err("轮转生产者的值在行尾已被同基名的另一次写盖住");
                }
                cur
            } else {
                let prev_home = cells
                    .iter()
                    .find(|o| o.q == c.q && o.lag == c.lag - 1)
                    .map(|o| o.home.clone())
                    .ok_or("轮转拷贝链缺一级格子")?;
                slot.get(&prev_home).copied().unwrap_or(None)
            };
            slot.insert(c.home.clone(), src);
        }
        Ok(())
    };
    // 发射一条实例：先按当前格子校验它的每个读，再把它自己的值压进基名槽。
    macro_rules! fire {
        ($p:expr, $k:expr, $s:expr) => {{
            let (p, k) = ($p, $k);
            if p >= n {
                return Err("实例引用了不存在的节点");
            }
            if k >= trip {
                // 与 `apply_into` 里的命名阶段用同一句话：这条越界在两侧都会被拦，
                // 原因文案不得分叉，否则测试钉的是文案而不是行为。
                return Err("实例迭代号超出 trip 范围");
            }
            for v in kind_uses(&lp.nodes[p].kind) {
                sim_check_read(lp, &def_of, &plan.shift, cells, &v, k, $s, &slot)?;
            }
            if let Some(b) = &base_of[p] {
                slot.insert(b.clone(), Some((p, k)));
            }
        }};
    }
    let mut seeded = vec![false; cells.len()];
    for (i, c) in cells.iter().enumerate() {
        if want_of(c) < 0 {
            seed_one(&mut slot, c)?;
            seeded[i] = true;
        }
    }
    for &(p, k) in &plan.prologue {
        fire!(p, k, None);
        for (i, c) in cells.iter().enumerate() {
            if !seeded[i] && c.q == p && want_of(c) == k as i64 {
                seed_one(&mut slot, c)?;
                seeded[i] = true;
            }
        }
    }
    if seeded.iter().any(|&s| !s) {
        // 与 `apply_into` 同一句话：那里的播种指令找不到插入点，落地就是拿旧值发上去。
        return Err("轮转格的播种实例不在 prologue 里");
    }
    for r in 0..rounds {
        // head 块：判定在 body 之前跑，读的是上一轮的产出 ⇒ 按 shift=0 的消费者算。
        for v in kind_uses(&lp.cond.kind) {
            sim_check_read(lp, &def_of, &plan.shift, cells, &v, r + cs, Some(cs), &slot)?;
        }
        for &p in &row {
            fire!(p, r + plan.shift[p], Some(plan.shift[p]));
        }
        shift_cells(&mut slot, r)?;
    }
    // 跳出循环的那一次判定同样在 head 里发射，它读的也必须是真实存在的实例。
    for v in kind_uses(&lp.cond.kind) {
        sim_check_read(lp, &def_of, &plan.shift, cells, &v, rounds + cs, Some(cs), &slot)?;
    }
    for &(p, k) in &plan.epilogue {
        fire!(p, k, None);
    }
    Ok(())
}

#[cfg(test)]
mod emit_order_tests {
    use super::*;
    use crate::ssa::Function;

    /// 最小错值形态（`tests/fixtures/opt/ssa_pipe_chain.c` 的 chain6_10）：6 级
    /// 依赖链、trip=10 —— 「区域节点数 ≥ 6 才触发」的那个形状。
    const CHAIN6: &str = "int c6(void){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<10;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f;}\n";

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

    /// 旧规则的行序：只看 `stage%ii`，不看基名塌缩后的反依赖。
    fn naive_row(stage: &[usize], ii: usize, n: usize) -> Vec<usize> {
        let mut row: Vec<usize> = (0..n).collect();
        row.sort_by_key(|&p| (stage[p] % ii, p));
        row
    }

    fn node_of(lp: &PipelinedLoop, base: &str) -> usize {
        lp.nodes
            .iter()
            .position(|n| n.dest.as_ref().map(|d| d.base.as_str()) == Some(base))
            .unwrap_or_else(|| panic!("调度区里没有以 {base} 为基名的节点"))
    }

    /// 该形状必须被接受，而且行序必须把链尾 `f += e` 排在 `e += d` 之前：
    /// f 要的是上一轮的 e，而机器码里那就是一个槽，晚一拍就被本轮的写盖掉。
    #[test]
    fn minimal_wrong_shape_gets_a_row_order_that_reads_the_right_slot() {
        let f = fn_ssa(CHAIN6, "c6");
        let p = plan_for_header(&f, "for.body").unwrap_or_else(|e| panic!("chain6 应过闸门：{e}"));
        emitted_reads_match(&p.lp, &p.inst).expect("接受的计划必须过发射序自检");
        let row = p.inst.row().expect("行序必须合法").clone();
        let pos = |q: usize| row.iter().position(|&x| x == q).unwrap();
        let (e_n, f_n) = (node_of(&p.lp, "e"), node_of(&p.lp, "f"));
        assert!(pos(f_n) < pos(e_n), "链尾必须先于它读的那个生产者：行序={row:?}");
    }

    /// 反向对照：把行序退回旧规则，发射序自检必须拒它，落地层也必须拒它。
    /// 没有这条，上一条可能只是因为自检空转才绿。
    #[test]
    fn unrefined_row_order_is_rejected() {
        let f = fn_ssa(CHAIN6, "c6");
        let p = plan_for_header(&f, "for.body").expect("chain6 应过闸门");
        let naive = naive_row(&p.stage, p.ii, p.lp.nodes.len());
        let bad = p
            .inst
            .with_row(&p.stage, &naive)
            .expect("旧行序仍是合法切分");
        let e = emitted_reads_match(&p.lp, &bad).expect_err("旧行序必须被发射序自检拒掉");
        assert!(e.contains("不是模型指定的实例"), "实得：{e}");
        // 「被拒时原 IR 必须逐字不变」这条断言靠的就是副本独立 —— 显式 `fork()`。
        let mut g = f.fork();
        let before = crate::ssa::dump_function(&g);
        assert!(apply_plan(&mut g, &p.lp, &bad).is_err(), "apply 不得接受被拒的行序");
        assert_eq!(crate::ssa::dump_function(&g), before, "被拒时原 IR 必须逐字不变");
    }

    /// 每个节点的实际执行次数必须恰为 trip（§7.5 要求的那条 IR 层断言）。
    #[test]
    fn every_node_executes_exactly_trip_times() {
        let f = fn_ssa(CHAIN6, "c6");
        let p = plan_for_header(&f, "for.body").expect("chain6 应过闸门");
        let want = vec![p.lp.trip; p.lp.nodes.len()];
        assert_eq!(p.inst.exec_counts(), want, "每节点恰执行 trip 次");
        // 反向对照：少一个排空实例就立即不等 —— 证明这条断言真的在数东西。
        let mut bad = p.inst.clone();
        bad.epilogue.pop();
        assert_ne!(bad.exec_counts(), want);
    }
}

/// 发射序两道判据（先按行内反依赖重排平局，再沿实际发射序模拟槽位内容）。
///
/// 两个发射模型共用它：拿不到合法行序就换下一个间隔 —— 更大的 II 会给出不同的
/// 平局，可能反而能排开。
fn emit_check(
    lp: &PipelinedLoop,
    inst: &InstancePlan,
    stage: &[usize],
) -> Result<InstancePlan, &'static str> {
    let refined = refine_kernel_row(lp, inst, stage)?;
    emitted_reads_match(lp, &refined)?;
    Ok(refined)
}

/// 唯一闸门：识别 → 建图 → MII → 模调度 → 三段切分 → 收益与代价判据。
///
/// 必须只有一条路径。分成两条（比如诊断走一套、实跑走另一套）的后果就是
/// 「测试绿而实际跳过」，或反过来在实际里跑了一个测试没覆盖的宽松版本。
///
/// **两个发射模型按固定顺序试，而不是合成一套判据**：
///   1. 基名塌缩模型（一个基名一个槽，`cells` 为空）——今天的行为，一字不改地
///      先试；它过的循环与新模型无关 ⇒ 已有语料不会因本轮抖动产物（判据：三档
///      哈希只允许「新增被接受的循环」那一类行变）。
///   2. 真轮转模型（每个 (生产者, lag) 一个自己的基名、一个自己的家）——只在模型 1
///      拿不到合法行序/合法读数时才试，目的就是解除「同基名无法同时保留两份
///      ⇒ 保守拒收」。
///
/// 代价判据：模型 1 只做**一级**轮转寄存器（`depth ≤ 1`），因为落地的形式是头块
/// 上一对 phi（上一轮 / 本轮）；模型 2 把那条换成上限 `MAX_ROT_DEPTH`/`MAX_ROT_CELLS`
/// （级数 N 时 prologue/epilogue 各多 N 轮、每轮多一条拷贝链，代价真的会盖过收益，
/// 那部分继续拒）。`DCC_SSA_OFF=rot` 就是关掉模型 2 ⇒ 逐字退回旧行为。
pub fn plan_for_header_with(
    func: &Function,
    header: &str,
    res: &Resources,
) -> Result<PipelinePlan, &'static str> {
    let lp = classify_loop(func, header)?;
    // 编译代价闸门：实例总数 = 节点数 × trip 线性于 trip，trip 极大时
    // （静态计数的大循环）`instance_plan` 会逐趟枚举上亿个实例，把
    // 编译器拖到分钟级/挂起（实测 trip=5e7 时 -O2 卡死）。超上限在建依赖图
    // 之前直接放弃流水。checked_mul 溢出也视为超限拒绝。
    if lp
        .trip
        .checked_mul(lp.nodes.len().max(1))
        .map_or(true, |n| n > MAX_PIPE_INSTANCES)
    {
        return Err("trip×节点数过大：放弃流水以限制编译代价");
    }
    let g = build_dep_graph(&lp.nodes, func, &func.blocks[header], &lp.phi_sources);
    let mii = rec_mii(&g).max(res_mii(&g, res));
    // 从 MII 起逐步增大；再大到节点数就没有重叠可言了。
    let ceiling = mii.max(lp.nodes.len());
    let rot_on = !crate::ssa::passes::ssa_dbg_off("rot");
    // 发射层拒绝原因比「没有可行调度」有用得多（行序成环 / 读到错实例），
    // 搜索耗尽时把它报出去，不让它被一个泛化原因掩盖。
    let mut last_emit: Option<&'static str> = None;
    // 真轮转自己的拒因（类型不可拷 / 级数与格数上限 / 拷贝链与跳轮读不一致）。
    // 只有模型 2 真的被试过才非 `None`，此时它才是诚实的那一条 —— 旧措辞
    // 「需要超过一级轮转寄存器（本层不做）」在本层已经做了，不能继续报它。
    let mut last_rot: Option<&'static str> = None;
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
        // ============ 模型 1：基名塌缩 ============
        match benefit_verdict(depth, ii, lp.trip) {
            Err(e) => {
                // `depth=0` / `trip 太短` 两条在两个模型下逐字相同（换更大的 II 只会
                // 更没意义）⇒ 就地结束并报原因，与旧行为同一个出口；关掉真轮转时
                // 连 `depth>1` 也照旧就地结束（那就是今天的行为，反向对照靠它）。
                //
                // 但报出去的那条必须是**更宽松那一档**的答案：`depth>1` 在模型 1 是
                // 「需要超过一级轮转寄存器（本层不做）」，而模型 2 早就不看这条，于是
                // 「depth=2 且 trip 不够」的循环若仍报前者，拒因就在指一条已经不存在的
                // 拦路虎（实测：同一形状把 trip 从 32 改成 4 仍报「需要超过一级轮转寄存器」，
                // 而真原因是 `trip 太短`）。`DCC_SSA_OFF=rot` 那一路逐字保持旧文案。
                if !rot_on {
                    return Err(e);
                }
                match benefit_verdict_rot(depth, ii, lp.trip, 0) {
                    Err(e2) => return Err(e2),
                    Ok(()) => last_emit = Some(e),
                }
            },
            Ok(()) => match emit_check(&lp, &inst, &stage) {
                Ok(p) => {
                    return Ok(PipelinePlan {
                        lp,
                        ii,
                        stage,
                        inst: p,
                    })
                },
                Err(e) => last_emit = Some(e),
            },
        }
        if !rot_on {
            continue;
        }
        // ============ 模型 2：真轮转 ============
        // 格子表在这里算一次并塞进计划：行内约束、发射序模拟、落地三处读的是
        // **同一份**答案（否则一边按「有格子」放行、另一边按「同一个槽」校验）。
        let rot = (|| -> Result<InstancePlan, &'static str> {
            let cells = needed_cells(func, &lp, &inst)?;
            let mut with_cells = inst.clone();
            with_cells.cells = cells;
            benefit_verdict_rot(depth, ii, lp.trip, with_cells.cells.len())?;
            emit_check(&lp, &with_cells, &stage)
        })();
        match rot {
            Ok(p) => {
                return Ok(PipelinePlan {
                    lp,
                    ii,
                    stage,
                    inst: p,
                })
            },
            Err(e) => last_rot = Some(e),
        }
    }
    Err(last_rot
        .or(last_emit)
        .unwrap_or("MII 到节点数之间找不到可行调度"))
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

    /// 依赖链循环：闸门必须给出一级重叠、且计划覆盖全部实例并与图自洽。
    /// load 归约实测 depth=0（已由 `load_reduction_has_no_cross_iteration_overlap`
    /// 钉住「必须拒」），所以它不能当接受样本。
    #[test]
    fn gate_accepts_the_dependency_chain() {
        let src = "int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}\n";
        let f = fn_ssa(src, "ch");
        let p = plan_for_header(&f, "for.body").unwrap_or_else(|e| panic!("依赖链应过闸门：{e}"));
        assert_eq!(p.inst.depth, 1);
        assert!(p.ii >= 2, "重叠靠链长拉开，II 至少应为 2");
        assert_eq!(p.inst.kernel.len() + p.inst.depth, p.lp.trip);
        assert_eq!(p.inst.flatten().len(), p.lp.nodes.len() * p.lp.trip);
        // 计划与图必须自洽：逐边校验。
        let g = build_dep_graph(&p.lp.nodes, &f, &f.blocks["for.body"], &p.lp.phi_sources);
        assert!(edges_satisfied(&g, p.ii, &p.stage));
        // 同一入口给出的原因清单里不能再有 for.body。
        assert!(rejections(&f, &Resources::default())
            .into_iter()
            .all(|(n, _)| n != "for.body"));
    }

    /// 分类器必须认得出「体内只写内存、没有标量递推」这类循环：它的结构（单 latch、
    /// 直线链、常量 trip）完全合法。旧版拿支配关系当环内判据、拿「头只有唯一后继」
    /// 当形状假设，会把整类循环默默丢掉。
    #[test]
    fn classifier_recognizes_pure_store_loop() {
        let src = "int wr(int n,int*a){for(int i=0;i<8;i++)a[i]=i;return n;}\n";
        let f = fn_ssa(src, "wr");
        let lp = classify_loop(&f, "for.body")
            .unwrap_or_else(|e| panic!("纯写循环的结构合法，分类器不该拒：{e}"));
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
            let rej: Vec<String> = rejections(&f, &Resources::default())
                .into_iter()
                .map(|(b, _)| b)
                .collect();
            let got: Vec<String> = plans_for(&f, &Resources::default())
                .into_iter()
                .map(|p| p.lp.header.clone())
                .collect();
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

    // ===== 真轮转（Phase 6.3）：解除「同基名无法同时保留两份 ⇒ 保守拒收」 =====

    /// 依赖链每一级都是一次数组载入 ⇒ 载入延迟把 stage 拉到 depth≥2。旧判据
    /// (`benefit_verdict` 的 `depth > 1`) 整条拒收它；现在每个跳轮读有自己的格子。
    ///
    /// 这条单测钉的是**计划层**：接受 + 格子表非空 + 名字互不相同且不与已有局部同名
    /// + 实例覆盖仍完整。产物层与执行层由 `tests/integration/p16_rotation.rs` 与夹具钉。
    #[test]
    fn gate_accepts_depth_two_load_chain_with_real_cells() {
        let src = "static int idx[8];\n\
                   int rl(int n){int s=0;for(int i=0;i<32;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;int c=idx[b&7]+1;int d=idx[c&7]+1;s+=d;}return s+n;}\n";
        let f = fn_ssa(src, "rl");
        let p = plan_for_header(&f, "for.body")
            .unwrap_or_else(|e| panic!("depth>1 的依赖载入链应被真轮转接受，实际被拒：{e}"));
        assert!(
            p.inst.depth >= 2,
            "样本必须真的跨过一级（否则测不到旧判据）：depth={}",
            p.inst.depth
        );
        assert!(
            !p.inst.cells.is_empty(),
            "接受了 depth≥2 却没有格子表 ⇒ 走的是塌缩模型，判据形同虚设"
        );

        // 每格一个 (生产者, lag)，且 home 互不相同 —— 「同一个基名的 K 份值各有位置」
        // 的全部含义。
        let mut keys = std::collections::BTreeSet::new();
        let mut homes = std::collections::BTreeSet::new();
        for c in &p.inst.cells {
            assert!(c.lag >= 1, "格子只服务跳 ≥ 1 轮的读：{:?}", c);
            assert!(
                keys.insert((c.q, c.lag)),
                "同一 (生产者, lag) 被分配了两个格子：{:?}",
                p.inst.cells
            );
            assert!(homes.insert(c.home.clone()), "两个格子拿到同一个家：{:?}", p.inst.cells);
        }
        // 名字必须是共享分配器的产物（`{base}_s{k}`），且不得与任何已有基名同名。
        let reserved = crate::ssa::location::LocMap::reserved_names(&f);
        for c in &p.inst.cells {
            assert!(
                c.home.contains("_s"),
                "格子名应由 `reserve_split_name` 给出 `{{base}}_s{{k}}`，实际 {}",
                c.home
            );
            assert!(!reserved.contains(&c.home), "格子名 {} 与已有局部/形参/phi dest 同名", c.home);
        }
        // 计划的实例覆盖与图自洽性不因格子而改变（格子只换存储位置，不换时序）。
        assert!(p.inst.verify(&p.stage), "带格子的计划必须仍逐实例覆盖");
        assert_eq!(p.inst.flatten().len(), p.lp.nodes.len() * p.lp.trip);
        let g = build_dep_graph(&p.lp.nodes, &f, &f.blocks["for.body"], &p.lp.phi_sources);
        assert!(edges_satisfied(&g, p.ii, &p.stage));
        // 发射序模拟对格子模型同样必须放行（否则闸门与自检两套真源）。
        emitted_reads_match(&p.lp, &p.inst).unwrap_or_else(|e| panic!("格子表与发射序不一致：{e}"));
    }

    /// 格子命名必须走全仓唯一的占名分配器：用户自己有一个叫 `i_s1` 的局部，而归纳
    /// 变量 `i` 的格子恰好想要 `i_s1` ⇒ 必须避让到 `i_s2`。绕过它就是两个局部同名。
    #[test]
    fn rotation_cells_yield_to_existing_user_locals() {
        let src = "static int idx[8];\n\
                   int clash(int n){int s=0,i_s1=5;for(int i=0;i<24;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;int c=idx[b&7]+1;int d=idx[c&7]+1;s+=d;s+=i_s1;}return s+n;}\n";
        let f = fn_ssa(src, "clash");
        let p = plan_for_header(&f, "for.body").expect("占名不应改变接受结论，只改变格子的名字");
        let iv_cell = p.inst.cells.iter().find(|c| c.home.starts_with("i_s"));
        let Some(c) = iv_cell else {
            // 形状变了就可能不需要 `i` 的格子（判定读的是别的实例）；那种情况下
            // 至少不能出现「格子名 = 用户局部名」这条越界。
            for x in &p.inst.cells {
                assert_ne!(x.home, "i_s1", "格子拿了用户已有名");
            }
            return;
        };
        assert_eq!(c.home, "i_s2", "`i_s1` 已被用户局部占用，格子必须避让：{:?}", p.inst.cells);
    }

    /// 判定指令的轮偏移是 `depth - 1`，不是常量 0。`depth ≤ 1` 时两者相同，所以
    /// 塌缩模型从来暴露不了它；写回 0 会让 depth=2 的循环多跑一整行（实测 375 变 391）。
    #[test]
    fn cond_shift_tracks_the_rotation_depth() {
        assert_eq!(cond_shift(0), 0);
        assert_eq!(cond_shift(1), 0, "depth=1 必须与旧行为逐字相同（产物字节不因此抖动）");
        assert_eq!(cond_shift(2), 1);
        assert_eq!(cond_shift(5), 4);
    }

    /// 轮转路径的四条代价判据都要能单独挨到：`depth=0` 与 `trip 太短` 两条文案与
    /// 塌缩模型一致（这样拒因在两模型下可对比），后两条才是轮转自己的上限。
    #[test]
    fn benefit_verdict_rot_covers_each_rule() {
        assert_eq!(
            benefit_verdict_rot(0, 3, 8, 4),
            Err("无跳迭代重叠 (depth=0)：流水只是搬动指令")
        );
        assert_eq!(
            benefit_verdict_rot(2, 3, 5, 4),
            Err("trip 太短：prologue/epilogue 开销盖过收益")
        );
        assert_eq!(
            benefit_verdict_rot(MAX_ROT_DEPTH + 1, 3, 64, 0),
            Err("轮转级数超上限：prologue/epilogue 代价盖过收益")
        );
        assert_eq!(
            benefit_verdict_rot(2, 3, 64, MAX_ROT_CELLS + 1),
            Err("轮转格数超上限：每轮行尾的拷贝链太长")
        );
        assert!(benefit_verdict_rot(2, 3, 8, 4).is_ok());
        // 旧判据在 depth>1 上仍然给那条文案：它就是反向对照要退回的地方。
        assert_eq!(benefit_verdict(2, 3, 64), Err("需要超过一级轮转寄存器（本层不做）"));
    }
}

// ======================================================= 实例物化（落地地基）

/// 物化出来的线性序列。
#[derive(Clone, Debug)]
pub struct Materialized {
    pub insts: Vec<Inst>,
    /// `(节点, 迭代)` → 该实例在序列里的输出名。
    pub names: HashMap<(usize, usize), String>,
}

struct ResolveCtx<'a> {
    lp: &'a PipelinedLoop,
    def_of: &'a HashMap<String, usize>,
    phi_from_pre: &'a HashMap<String, Operand>,
    external: &'a HashSet<String>,
}

/// 把「节点 p 在迭代 k」这条指令里的一个源操作数解析成生产它的那个实例的输出名。
///
/// 三种情形必须分开：
///   * 读的是**循环携带值**（头的 phi dest）⇒ 要的是上一轮的生产者，即
///     `(phi_sources[v], k-1)`；第 0 轮读的是进循环前的值（phi 的 preheader 侧）。
///   * 读的是**区域内**其它定义 ⇒ 同一迭代的那个实例。
///   * 读的是**区域外**（参数、全局、循环前的定义）⇒ 原样保留。
/// 任何「生产者还没物化」的情形都是错误返回，绝不留下一个前向引用。
fn resolve_operand(
    o: &Operand,
    k: usize,
    ctx: &ResolveCtx,
    names: &HashMap<(usize, usize), String>,
) -> Result<Operand, &'static str> {
    let Operand::Var(v) = o else {
        return Ok(o.clone());
    };
    let key = v.name();
    if let Some(&q) = ctx.lp.phi_sources.get(&key) {
        if k == 0 {
            let src = ctx
                .phi_from_pre
                .get(&key)
                .ok_or("phi 没有 preheader 侧输入")?;
            return Ok(src.clone());
        }
        let name = names
            .get(&(q, k - 1))
            .cloned()
            .ok_or("携带值的上一轮生产者尚未物化（调度序与依赖矛盾）")?;
        return Ok(Operand::Var(SsaVar {
            base: name,
            version: 0,
            ty: v.ty.clone(),
            span: v.span.clone(),
        }));
    }
    if let Some(&q) = ctx.def_of.get(&key) {
        let name = names
            .get(&(q, k))
            .cloned()
            .ok_or("同迭代依赖的生产者尚未物化（实例序颠倒了）")?;
        return Ok(Operand::Var(SsaVar {
            base: name,
            version: 0,
            ty: v.ty.clone(),
            span: v.span.clone(),
        }));
    }
    if ctx.external.contains(&key) {
        return Ok(o.clone());
    }
    Err("引用了既不在调度区内、也不在区域外的名字")
}

/// 穷举重写一条指令里的所有读操作数。match 覆盖 `InstKind` 的全部变体是编译
/// 器强制的 —— 新增变体时漏处理会直接编不过，而不是静默漏掉一条依赖。
fn rewrite_kind<F>(k: &InstKind, f: &F) -> Result<InstKind, &'static str>
where
    F: Fn(&Operand) -> Result<Operand, &'static str>,
{
    Ok(match k {
        InstKind::Bin { op, l, r } => InstKind::Bin {
            op: *op,
            l: f(l)?,
            r: f(r)?,
        },
        InstKind::Un { op, e } => InstKind::Un { op: *op, e: f(e)? },
        InstKind::Cast { ty, e } => InstKind::Cast {
            ty: ty.clone(),
            e: f(e)?,
        },
        InstKind::AddrOf { base } => InstKind::AddrOf { base: base.clone() },
        InstKind::Load { e, ty, is_volatile } => InstKind::Load {
            e: f(e)?,
            ty: ty.clone(),
            is_volatile: *is_volatile,
        },
        InstKind::Store {
            e,
            src,
            is_volatile,
        } => InstKind::Store {
            e: f(e)?,
            src: f(src)?,
            is_volatile: *is_volatile,
        },
        InstKind::IndexAddr { arr, idx, ty } => InstKind::IndexAddr {
            arr: f(arr)?,
            idx: f(idx)?,
            ty: ty.clone(),
        },
        InstKind::IndexLoad {
            arr,
            idx,
            ty,
            is_volatile,
        } => InstKind::IndexLoad {
            arr: f(arr)?,
            idx: f(idx)?,
            ty: ty.clone(),
            is_volatile: *is_volatile,
        },
        InstKind::IndexStore {
            arr,
            idx,
            src,
            ty,
            is_volatile,
        } => InstKind::IndexStore {
            arr: f(arr)?,
            idx: f(idx)?,
            src: f(src)?,
            ty: ty.clone(),
            is_volatile: *is_volatile,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => InstKind::Call {
            callee: f(callee)?,
            args: args.iter().map(f).collect::<Result<Vec<_>, _>>()?,
            ret_ty: ret_ty.clone(),
        },
        InstKind::MemberAddr {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
        } => InstKind::MemberAddr {
            obj: f(obj)?,
            obj_ty: obj_ty.clone(),
            field: field.clone(),
            ty: ty.clone(),
            is_arrow: *is_arrow,
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
            is_volatile,
            is_arrow,
        } => InstKind::MemberLoad {
            obj: f(obj)?,
            obj_ty: obj_ty.clone(),
            field: field.clone(),
            ty: ty.clone(),
            is_volatile: *is_volatile,
            is_arrow: *is_arrow,
        },
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
            ty,
            is_volatile,
            is_arrow,
        } => InstKind::MemberStore {
            obj: f(obj)?,
            obj_ty: obj_ty.clone(),
            field: field.clone(),
            src: f(src)?,
            ty: ty.clone(),
            is_volatile: *is_volatile,
            is_arrow: *is_arrow,
        },
        InstKind::Select { cond, then_, else_ } => InstKind::Select {
            cond: f(cond)?,
            then_: f(then_)?,
            else_: f(else_)?,
        },
        InstKind::InlineAsm { .. } => return Err("asm 指令不可物化（准入应已拒）"),
        InstKind::PreservedDecl { .. } => return Err("用户声明不可按迭代复制（准入应已拒）"),
        InstKind::Invoke { .. } => return Err("受保护调用不可物化（准入应已拒）"),
        InstKind::EhLandingPad { .. } => return Err("落地垫不可物化（准入应已拒）"),
    })
}

/// 把一个流水计划物化成**一条线性指令序列**：每个 `(节点, 迭代)` 实例复制一份，
/// 源操作数改写成生产它的实例的输出名，输名字统一成唯一的 `_pipe{seq}`。
///
/// 这是「应用到 IR」的地基：kernel 的轮转 phi 只是把这条序列压缩回循环的紧凑
/// 形式。序列本身的合法性是**可判定**的 —— 每个引用都必须指向更早的实例或区域外
/// 定义。模调度只要漏一条边（含基名塌缩序边、内存定序边、跳迭代边），这里就会以
/// 「生产者尚未物化」的形式炸出来，而不是以错值形式进入产物。
pub fn materialize(
    func: &Function,
    lp: &PipelinedLoop,
    plan: &InstancePlan,
) -> Result<Materialized, &'static str> {
    let n = lp.nodes.len();
    let mut def_of: HashMap<String, usize> = HashMap::new();
    for (i, inst) in lp.nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            def_of.insert(d.name(), i);
        }
    }
    let mut external: HashSet<String> = func.params.iter().map(|p| p.name()).collect();
    for name in crate::ssa::ordered_block_names(func) {
        if lp.region_bbs.contains(&name) {
            continue;
        }
        let Some(bb) = func.blocks.get(&name) else {
            continue;
        };
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                external.insert(d.name());
            }
        }
    }
    let mut phi_from_pre: HashMap<String, Operand> = HashMap::new();
    if let Some(h) = func.blocks.get(&lp.header) {
        for p in &h.phis {
            if let Some((src, _)) = p.incoming.iter().find(|(_, from)| *from == lp.preheader) {
                phi_from_pre.insert(p.dest.name(), Operand::Var(src.clone()));
            }
        }
    }
    let ctx = ResolveCtx {
        lp,
        def_of: &def_of,
        phi_from_pre: &phi_from_pre,
        external: &external,
    };

    let mut names: HashMap<(usize, usize), String> = HashMap::new();
    let mut insts: Vec<Inst> = Vec::new();
    for (idx, &(p, k)) in plan.flatten().iter().enumerate() {
        if p >= n {
            return Err("实例引用了不存在的节点");
        }
        let src = &lp.nodes[p];
        if !inst_is_materializable(&src.kind) {
            return Err("调度区内有不可按迭代复制的指令（异常边/用户声明/asm 输出）");
        }
        let out = format!("_pipe{idx}");
        let kind: InstKind;
        {
            let f = |o: &Operand| resolve_operand(o, k, &ctx, &names);
            kind = rewrite_kind(&src.kind, &f)?;
        }
        let dest = src.dest.as_ref().map(|d| SsaVar {
            base: out.clone(),
            version: 0,
            ty: d.ty.clone(),
            span: d.span.clone(),
        });
        insts.push(Inst {
            dest,
            kind,
            span: src.span.clone(),
        });
        names.insert((p, k), out);
    }
    // 覆盖性：每个 (节点,迭代) 都要有名字，且序列长度与实例数一致。
    if names.len() != n * (plan.kernel.len() + plan.depth) || insts.len() != plan.flatten().len() {
        return Err("物化序列的实例覆盖不完整");
    }
    Ok(Materialized { insts, names })
}

#[cfg(test)]
mod materialize_tests {
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

    // 目标形态是依赖链（load 归约实测 depth=0，闸门不接受，不能用作物化目标）。
    const RED: &str = "int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}\n";

    fn red_plan() -> (Function, PipelinedLoop, PipelinePlan) {
        let f = fn_ssa(RED, "ch");
        let p = plan_for_header(&f, "for.body").unwrap_or_else(|e| panic!("red 应过闸门：{e}"));
        let lp = p.lp.clone();
        (f, lp, p)
    }

    /// 真实循环的物化序列必须是**合法线性序**：每个 `_pipe*` 引用都指向更早的
    /// 实例。这就是「模调度没有漏边」的直接证据 —— 漏一条边这里就炸。
    #[test]
    fn materialized_sequence_has_no_forward_reference() {
        let (f, lp, p) = red_plan();
        let m = materialize(&f, &lp, &p.inst).unwrap_or_else(|e| panic!("物化失败：{e}"));
        assert_eq!(m.insts.len(), lp.nodes.len() * lp.trip, "实例数 = 节点数 × 迭代数");
        assert_eq!(m.names.len(), m.insts.len());
        let mut defined: HashSet<String> = HashSet::new();
        for (idx, inst) in m.insts.iter().enumerate() {
            for u in kind_uses(&inst.kind) {
                if u.base.starts_with("_pipe") {
                    assert!(defined.contains(&u.name()), "实例 {idx} 前向引用 {}", u.name());
                }
            }
            if let Some(d) = &inst.dest {
                assert!(d.base.starts_with("_pipe"), "输名字必须是唯一的合成名：{}", d.name());
                assert!(!defined.contains(&d.name()), "输名字不得重复");
                defined.insert(d.name());
            }
        }
        // 携带值确实跨轮引用（否则说明物化把递归拍平了，那是错的）。
        let crossed = m.names.iter().filter(|((_, k), _)| *k > 0).count();
        assert!(crossed > 0, "应有跨轮实例");
    }

    /// 负向对照：把 kernel 行内序颠倒，物化必须报错而不是产出一个前向引用序列。
    /// 没有这条，上一条测试可能因为「永远不会出现错序」而成为空判。
    #[test]
    fn materialize_rejects_reversed_row() {
        let (f, lp, p) = red_plan();
        let mut bad = p.inst.clone();
        for row in bad.kernel.iter_mut() {
            row.reverse();
        }
        let e = materialize(&f, &lp, &bad).expect_err("颠倒行内序必须被拒");
        assert!(e.contains("尚未物化"), "应报生产者缺失，实得：{e}");
    }

    /// 区域内有不可按迭代复制的指令（inline asm 的输出/易失标记）时，准入就该拒。
    #[test]
    fn admission_refuses_unmaterializable_insts() {
        let src = "int a(int n,int*v){int s=0;for(int i=0;i<8;i++){{__asm__ volatile(\"nop\");s+=v[i];}}return s;}\n";
        let f = fn_ssa(src, "a");
        let errs = rejections(&f, &Resources::default());
        assert!(
            errs.iter().any(|(_, e)| e.contains("不可按迭代复制")),
            "含 asm 的区域必须被准入拒掉：{errs:?}"
        );
    }
}

// ===================================================== 计划应用到 IR（落地）

/// 版本分配器：以「全函数现有最大版本」为起点递增，保证与构建器产出的名字不撞。
struct VersionAllocator {
    max: HashMap<String, u32>,
}

impl VersionAllocator {
    fn new(func: &Function) -> Self {
        let mut max: HashMap<String, u32> = HashMap::new();
        let note = |v: &SsaVar, max: &mut HashMap<String, u32>| {
            let e = max.entry(v.base.clone()).or_insert(0);
            if v.version > *e {
                *e = v.version;
            }
        };
        for p in &func.params {
            note(p, &mut max);
        }
        for bb in func.blocks.values() {
            for phi in &bb.phis {
                note(&phi.dest, &mut max);
            }
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    note(d, &mut max);
                }
            }
        }
        VersionAllocator { max }
    }
    fn fresh(&mut self, base: &str) -> u32 {
        let v = self.max.entry(base.to_string()).or_insert(0);
        *v += 1;
        *v
    }
}

/// 把一个流水计划**应用到 IR**：peel 出 prologue、把 kernel 重排成单块自然循环、
/// 把 epilogue 放到出口块头部。
///
/// kernel 为什么必须是单块：真实 kernel 行的节点在原 IR 里分属 `for.body` 与
/// `for.step`，而且**跨块交错**（`red` 的行序是 `[0,2,6,3,5,1,4]`，`for.body` 的节点
/// 1 排在 `for.step` 的节点之后）。两块的形状表达不出这个序，而序就是正确性
/// （`edges_satisfied` 保证的正是按 T 序串行执行）。所以落成 rebuild 已经支持的
/// 最简形状：一个带 phi、带条件分支的单块循环（与 `while.cond` 同形）。
///
/// 失败安全：全部工作在 `Function` 的副本上做，只有 `dangling_refs`（不得有悬空
/// 引用）与 `duplicate_ssa_defs`（全函数单赋值）都通过时才替换原函数。任何一条
/// 判据不满足就返回 `Err`，原 IR 一个字节都不动。
pub fn apply_plan(
    func: &mut Function,
    lp: &PipelinedLoop,
    plan: &InstancePlan,
) -> Result<(), &'static str> {
    // 在副本上落地、验证失败则原件不动 —— 整函数复制必须走显式 `fork()`（含 `value_loc`）。
    let mut work = func.fork();
    apply_into(&mut work, lp, plan)?;
    let dangling = crate::ssa::dangling_refs(&work);
    if !dangling.is_empty() {
        return Err("落地后存在悬空引用");
    }
    let dup = crate::ssa::duplicate_ssa_defs(&work);
    if !dup.is_empty() {
        return Err("落地后违反单赋值");
    }
    *func = work;
    Ok(())
}

fn apply_into(
    func: &mut Function,
    lp: &PipelinedLoop,
    plan: &InstancePlan,
) -> Result<(), &'static str> {
    let n = lp.nodes.len();
    // 0. 计划与函数必须匹配：区域拼接序、块集合、phi 集合都逐条核对。
    //    拿旧计划改新 IR 会产出自洽但错误的代码，这比编译失败更糟。
    let mut expect: Vec<Inst> = Vec::new();
    for name in &lp.region_bbs {
        let bb = func.blocks.get(name).ok_or("计划引用的块已不存在")?;
        expect.extend(bb.insts.iter().cloned());
    }
    // 拼接序里先剔除同一条判定指令，才能与 `lp.nodes`（调度区不含条件）对齐。
    let cond_dest = lp.cond.dest.as_ref().map(|d| d.name());
    if let Some(p) = expect
        .iter()
        .position(|i| i.dest.as_ref().map(|d| d.name()) == cond_dest)
    {
        expect.remove(p);
    }
    // 只比 kind（不比 span）：同一条指令换了位置仍是同样的语义。
    let same_kind =
        |a: &Inst, b: &Inst| -> bool { format!("{:?}", a.kind) == format!("{:?}", b.kind) };
    if expect.len() != n
        || !expect
            .iter()
            .zip(lp.nodes.iter())
            .all(|(a, b)| same_kind(a, b))
    {
        return Err("计划与当前 IR 不匹配（区域已变）");
    }
    let header = lp.header.clone();
    let latch = lp.latch.clone();
    let preheader = lp.preheader.clone();
    let exit = lp.exit.clone();
    if header == preheader || header == exit || latch == exit {
        return Err("循环角色重合");
    }
    // 发射序自检。闸门已过一次，这里再过一次不是重复：`apply_plan` 可以被直接调用
    // （单测、后续新接入点），而它发出的就是「基名同一个槽」的 IR —— 那个语义只有
    // 沿实际发射序模拟才判得出来，拿旧计划或手改过的行序进来都会在此被拦下。
    emitted_reads_match(lp, plan)?;

    let row = &plan.kernel[0];
    if row.len() != n {
        return Err("kernel 行不含全部节点");
    }
    let mut row_pos: HashMap<usize, usize> = HashMap::new();
    for (i, &p) in row.iter().enumerate() {
        if !row_pos.insert(p, i).is_none() {
            return Err("kernel 行内有重复节点");
        }
    }
    let shift = &plan.shift;
    let rounds = plan.kernel.len();
    let trip = rounds + plan.depth;

    // 1. 名字：kernel 每个节点一个 dest（每轮同一个名字），prologue/epilogue 每个
    //    实例一个 dest。版本序按物化序递增，保证「基名塌缩后」的赋值序与 T 序一致。
    let mut alloc = VersionAllocator::new(func);
    let mut kernel_name: Vec<Option<SsaVar>> = vec![None; n];
    let mut inst_name: HashMap<(usize, usize), SsaVar> = HashMap::new();
    for &(p, k) in plan.flatten().iter() {
        if k >= trip {
            // 实例的迭代号越界意味着切分与 trip 不自洽，绝不能继续命名。
            return Err("实例迭代号超出 trip 范围");
        }
        let src = &lp.nodes[p];
        let Some(d) = &src.dest else { continue };
        let v = alloc.fresh(&d.base);
        let nv = SsaVar {
            base: d.base.clone(),
            version: v,
            ty: d.ty.clone(),
            span: d.span.clone(),
        };
        if k >= shift[p] && k < shift[p] + rounds {
            // kernel 内的实例：每轮复用同一个名字，所以只能分配一次。
            if kernel_name[p].is_none() {
                kernel_name[p] = Some(nv.clone());
            }
            inst_name.insert((p, k), kernel_name[p].clone().unwrap());
        } else {
            inst_name.insert((p, k), nv);
        }
    }
    for p in 0..n {
        if kernel_name[p].is_none() != lp.nodes[p].dest.is_none() {
            return Err("kernel 实例命名与节点结构不一致");
        }
    }

    let mut def_of: HashMap<String, usize> = HashMap::new();
    for (i, inst) in lp.nodes.iter().enumerate() {
        if let Some(d) = &inst.dest {
            def_of.insert(d.name(), i);
        }
    }
    // 原 phi 的 preheader 侧输入（第 0 轮读携带值时用）。
    let mut phi_pre: HashMap<String, Operand> = HashMap::new();
    for p in &func.blocks[&header].phis {
        if let Some((src, _)) = p.incoming.iter().find(|(_, from)| *from == preheader) {
            phi_pre.insert(p.dest.name(), Operand::Var(src.clone()));
        }
    }

    // 2. kernel 需要的轮转 phi。跳轮数只由下面这一个公式给出，phi 生成与操作数解析
    //    都走它 —— 两处各写一套正是上一版「测试绿而实际错」的根源。
    //
    //    消费者 p 在轮 r 执行迭代 k_p = r + shift[p]。它读节点 q 在迭代 k_needed 的
    //    定义：携带值（头的 phi）要 k_p - 1，区域内其它定义要 k_p。而 q 在轮 r_q 产出
    //    迭代 r_q + shift[q] ⇒ r_q = k_needed - shift[q]，所以
    //        back = r - r_q = shift[q] - shift[p] + (携带?1:0)
    //    back = 0 ⇒ 本轮前向引用；back = 1 ⇒ 需要一级轮转 phi；其它 ⇒ 超过一级 ⇒ 拒。
    //
    //    这条公式两次出错都是被检查逼出来的：先只把 phi dest 当跳轮引用（漏了
    //    `节点 6 读 i_2` 其实是跳上一轮），后一次是把 shift 写反（back 算成 -1）。
    //    两者都会产生前向引用，而前向引用在产物里是错值，不是编译错误。
    //
    //    公式本体在 `back_rounds`（与本函数同一个），行序约束与这里共用它。
    // 初值所在的实例：第 0 轮读者要的是 (q, shift[q] - back)；`None` 表示读者是
    // khead 块里那条判定指令（它自己的轮偏移是 `cond_shift(depth)`，不占节点号）。
    let cs = cond_shift(plan.depth);
    let init_iter = |q: usize, cons: Option<usize>, carried: bool| -> i64 {
        let back = back_rounds(cons.map_or(cs, |p| shift[p]), shift[q], carried);
        (shift[q] as i64) - back
    };
    // 读者在 kernel 里应该读的「本轮起始值」名字。两个模型两套（但只一条跳轮公式）：
    //   * 塌缩：`rot_dest[q]` —— 生产者自己的基名的另一版（同一个槽）；
    //   * 真轮转：`cell_read[(q,lag)]` —— 格子的独立基名（自己的家）。
    let mut rot_dest: HashMap<usize, SsaVar> = HashMap::new();
    let mut cell_read: HashMap<(usize, i64), SsaVar> = HashMap::new();
    let mut kernel_phis: Vec<Phi> = Vec::new();
    // 轮转格的播种指令（`seed_first` = 读「进循环前的值」那些，必须排在 prologue
    // 之前；`seed_after` = 按它读的那个实例位置插入）与行尾拷贝链。
    let cells = plan.cells.clone();
    let mut seed_first: Vec<Inst> = Vec::new();
    let mut seed_after: HashMap<(usize, usize), Vec<Inst>> = HashMap::new();
    let mut row_copies: Vec<Inst> = Vec::new();
    if cells.is_empty() {
        // 塔缩模型只能表达一级轮转（头块上一对 phi：上一轮 / 本轮）。depth > 1 而没有格子
        // 意味着「行内序能护住但引用关系已经超出一级」的形状，走到这里就是上游判据改了
        // 而这里没跟进 ⇒ 必须点名拒，不能默默拿本轮的值当上一轮。
        if plan.depth > 1 {
            return Err("多级流水需要轮转格但格子表为空（上游判据与本处不一致）");
        }
        // 生产者节点 → (一个读者, 被读的名字, 是否携带值)
        let mut rot: HashMap<usize, (Option<usize>, String, bool)> = HashMap::new();
        for &p in row {
            for v in kind_uses(&lp.nodes[p].kind) {
                let key = v.name();
                let carried = lp.phi_sources.contains_key(&key);
                let src = if carried {
                    lp.phi_sources.get(&key)
                } else {
                    def_of.get(&key)
                };
                let Some(&q) = src else { continue };
                match back_rounds(shift[p], shift[q], carried) {
                    0 => {},
                    1 => {
                        // 同一生产者的多个读者必须落在同一个初值实例上，否则一个 phi
                        // 服不了两个读者。
                        if let Some((p2, _, co2)) = rot.insert(q, (Some(p), key.clone(), carried)) {
                            if init_iter(q, p2, co2) != init_iter(q, Some(p), carried) {
                                return Err("同一轮转生产者的初值实例不一致");
                            }
                        }
                    },
                    _ => return Err("跳轮引用需要超过一级轮转寄存器"),
                }
            }
        }
        // 判定指令也占一个位置：它在 body 之前执行（见下的 `khead` 块），所以它的「轮偏移」
        // 是 `cond_shift`；而它读的是 body 节点上一轮的输出 ⇒ 那些节点同样需要轮转 phi。
        // 不把它们收进来，head 就会引用一个尚未产出的名字。
        for v in kind_uses(&lp.cond.kind) {
            let key = v.name();
            let Some(&q) = lp.phi_sources.get(&key).or_else(|| def_of.get(&key)) else {
                continue;
            };
            let carried = lp.phi_sources.contains_key(&key);
            if back_rounds(cs, shift[q], carried) != 1 {
                return Err("判定所需的跳轮偏移不是一级轮转");
            }
            if let Some((p2, _, co2)) = rot.insert(q, (None, key.clone(), carried)) {
                if init_iter(q, p2, co2) != init_iter(q, None, carried) {
                    return Err("同一轮转生产者的初值实例不一致");
                }
            }
        }
        let mut rot_nodes: Vec<usize> = rot.keys().copied().collect();
        rot_nodes.sort_unstable();
        for &q in &rot_nodes {
            let (p, key, carried) = rot[&q].clone();
            let src_var = lp.nodes[q].dest.as_ref().ok_or("轮转生产者没有输出")?;
            let v = alloc.fresh(&src_var.base);
            let dest = SsaVar {
                base: src_var.base.clone(),
                version: v,
                ty: src_var.ty.clone(),
                span: src_var.span.clone(),
            };
            // 第 0 轮需要的实例；负数表示「进循环前的值」，用原 phi 的 preheader 侧输入。
            let want = init_iter(q, p, carried);
            let pre_val: Operand = if want < 0 {
                phi_pre
                    .get(&key)
                    .cloned()
                    .ok_or("phi 没有 preheader 侧输入")?
            } else {
                inst_name
                    .get(&(q, want as usize))
                    .cloned()
                    .map(Operand::Var)
                    .ok_or("轮转 phi 的初值实例没有被物化")?
            };
            let lat_val = kernel_name[q].clone().ok_or("轮转生产者不在 kernel 内")?;
            rot_dest.insert(q, dest.clone());
            kernel_phis.push(Phi {
                dest,
                incoming: vec![
                    (
                        match pre_val {
                            Operand::Var(x) => x,
                            _ => return Err("轮转 phi 的初值必须是变量"),
                        },
                        preheader.clone(),
                    ),
                    (lat_val, header.clone()),
                ],
            });
        }
    } else {
        // ---------- 分支 B：真轮转（每个 (生产者, lag) 一个独立基名 = 一个自己的家） ----------
        //
        // 三件事必须同时成立，缺一件就是静默错值：
        //   1. 每格一个 khead phi，dest 与两个 incoming **同基名**（`home`）。重建
        //      (`rebuild_cycle`) 把头块 phi 的 dest 别名到第一个 incoming，只有同基名
        //      才能保证「读写落到同一个家」在基名塌缩后仍成立；
        //   2. 播种指令必须插在**它读的那个实例之后**（`want < 0` 时插在 prologue 之前），
        //      因为 `a.v` 这类实例名在重建后就是槽 `a`：早一步拿到的是进循环前的值，
        //      晚一步拿到的是后续轮的值；
        //   3. 行尾拷贝链按 lag **从大到小**发（`a_s3 = a_s2; a_s2 = a_s1; a_s1 = a`），
        //      从小到大会把本轮的新值一路灌进每一格。
        // 名字来自 `needed_cells`（`LocMap::reserve_split_name` 的共享分配器），这里只
        // 照抄不再分配一次；格子基名同时进 `original_locals`（声明 + 不被 DCE + 调度
        // 的基名序边都认这个名字）。
        let src_of = |q: usize| -> Result<&SsaVar, &'static str> {
            lp.nodes[q].dest.as_ref().ok_or("轮转生产者没有输出")
        };
        let mk = |home: &str, src: &SsaVar, alloc: &mut VersionAllocator| -> SsaVar {
            let v = alloc.fresh(home);
            SsaVar {
                base: home.to_string(),
                version: v,
                ty: src.ty.clone(),
                span: src.span.clone(),
            }
        };
        // 1. 每格一个 phi dest（行尾拷贝与读者都引用它）。`cells` 已是 (q asc, lag asc)
        //    的确定序，HashMap 的随机序不得泄进名字版本（它直接决定产物字节）。
        for c in cells.iter() {
            let src = src_of(c.q)?;
            let dest = mk(&c.home, src, &mut alloc);
            if cell_read.insert((c.q, c.lag), dest.clone()).is_some() {
                return Err("轮转格重复（同一 (生产者, lag) 被分配了两次）");
            }
            kernel_phis.push(Phi {
                dest,
                incoming: Vec::new(),
            });
        }
        // 2. 播种：`want = shift[q] - lag`（与 `seed_cells` 、`init_iter` 同一个公式）。
        let mut seed_name: HashMap<(usize, i64), SsaVar> = HashMap::new();
        for c in cells.iter() {
            let src = src_of(c.q)?;
            let want = shift[c.q] as i64 - c.lag;
            let val: Operand = if want < 0 {
                // 「进循环前的那份值」只能从一个确定的携带 phi 拿（`needed_cells` 已保证
                // 同一格的全部负下标读共享这个名字）；名字不确定的格子在闸门里就被拒了。
                let sk = c
                    .seed
                    .as_ref()
                    .ok_or("轮转格初值是进循环前的值，但没有确定的携带名")?;
                phi_pre
                    .get(sk)
                    .cloned()
                    .ok_or("phi 没有 preheader 侧输入")?
            } else {
                inst_name
                    .get(&(c.q, want as usize))
                    .cloned()
                    .map(Operand::Var)
                    .ok_or("轮转格的初值实例没有被物化")?
            };
            let Operand::Var(x) = val else {
                return Err("轮转格的初值必须是变量");
            };
            let dest = mk(&c.home, src, &mut alloc);
            seed_name.insert((c.q, c.lag), dest.clone());
            let inst = Inst {
                dest: Some(dest),
                // 同型 `Cast` = 原样搬运（实测发 `mov`/`fldt`+`fstpt`，不是 `+0` 那条）。
                kind: InstKind::Cast {
                    ty: src.ty.clone(),
                    e: Operand::Var(x),
                },
                span: src.span.clone(),
            };
            if want < 0 {
                seed_first.push(inst);
            } else {
                seed_after
                    .entry((c.q, want as usize))
                    .or_default()
                    .push(inst);
            }
        }
        // 3. 行尾拷贝链：先按 (q asc, lag desc) 把名字分配完（后面的要引用前面的 dest，
        //    而发射序就是这里的序），再拼指令。
        let mut chain: Vec<&RotCell> = cells.iter().collect();
        chain.sort_by(|a, b| a.q.cmp(&b.q).then(b.lag.cmp(&a.lag)));
        let mut copy_name: HashMap<(usize, i64), SsaVar> = HashMap::new();
        for c in chain.iter() {
            let src = src_of(c.q)?;
            copy_name.insert((c.q, c.lag), mk(&c.home, src, &mut alloc));
        }
        for c in chain.iter() {
            let src = src_of(c.q)?;
            let from: Operand = if c.lag == 1 {
                Operand::Var(kernel_name[c.q].clone().ok_or("轮转生产者不在 kernel 内")?)
            } else {
                cell_read
                    .get(&(c.q, c.lag - 1))
                    .cloned()
                    .map(Operand::Var)
                    .ok_or("轮转拷贝链缺一级格子")?
            };
            let Operand::Var(x) = from else {
                return Err("轮转拷贝的源必须是变量");
            };
            let dest = copy_name
                .get(&(c.q, c.lag))
                .cloned()
                .ok_or("轮转拷贝没有名字")?;
            row_copies.push(Inst {
                dest: Some(dest),
                kind: InstKind::Cast {
                    ty: src.ty.clone(),
                    e: Operand::Var(x),
                },
                span: src.span.clone(),
            });
        }
        // 4. 把 phi 的两个 incoming 回填（同基名：见上面第 1 条）。
        for (i, c) in cells.iter().enumerate() {
            let phi = &mut kernel_phis[i];
            let dest = cell_read
                .get(&(c.q, c.lag))
                .cloned()
                .ok_or("轮转格没有 phi")?;
            if phi.dest != dest {
                return Err("轮转格 phi 与回填不同名（格子表与 phi 表失同）");
            }
            phi.incoming = vec![
                (
                    seed_name
                        .get(&(c.q, c.lag))
                        .cloned()
                        .ok_or("轮转格没有播种指令")?,
                    preheader.clone(),
                ),
                (
                    copy_name
                        .get(&(c.q, c.lag))
                        .cloned()
                        .ok_or("轮转格没有行尾拷贝")?,
                    header.clone(),
                ),
            ];
        }
        // 5. 格子基名当用户局部看：重建靠它声明局部，`dead_inst_elim` 靠它不删赋值，
        //    `instruction_scheduling` 的基名序边也只对具名基名生数——少了最后这条，
        //    -O3 会把行尾拷贝提前到生产者之前（静默错值）。
        for c in cells.iter() {
            // `OrderedNameSet::insert` 本身就是「没有才加、保持首次插入序」，所以这里
            // 不能自己写一份 `contains` + `push`：那会绕过它的保序去重口径。
            func.original_locals.insert(c.home.clone());
        }
    }

    // 3. 逐实例生成指令。
    let resolve = |o: &Operand, p: usize, k: Option<usize>| -> Result<Operand, &'static str> {
        let Operand::Var(x) = o else {
            return Ok(o.clone());
        };
        let key = x.name();
        let (q, carried) = if let Some(&q) = lp.phi_sources.get(&key) {
            (q, true)
        } else if let Some(&q) = def_of.get(&key) {
            (q, false)
        } else {
            // 区域外（参数、全局、循环前的定义）：原样保留。
            return Ok(o.clone());
        };
        match k {
            // prologue / epilogue：解析到具体实例。
            Some(k) => {
                let want = k as i64 - if carried { 1 } else { 0 };
                if want < 0 {
                    return Ok(phi_pre
                        .get(&key)
                        .cloned()
                        .ok_or("phi 没有 preheader 侧输入")?);
                }
                let sv = inst_name
                    .get(&(q, want as usize))
                    .cloned()
                    .ok_or("依赖的生产者实例尚未物化")?;
                // prologue 在 kernel 之前执行，不得引用 kernel 内的名字；epilogue 在
                // kernel 之后，引用它正好合法。
                if k < shift[p] && want >= shift[q] as i64 && want < (shift[q] + rounds) as i64 {
                    return Err("prologue 引用了 kernel 内的实例（值在尚未执行时就被读）");
                }
                Ok(Operand::Var(sv))
            },
            // kernel：back == 0 ⇒ 本轮前向（要求行内早于消费者）；back ≥ 1 ⇒ 要么有
            // 自己的轮转格（读格子的 phi），要么塌缩模型里那对一级 phi。
            None => {
                let back = back_rounds(shift[p], shift[q], carried);
                if let Some(d) = cell_read.get(&(q, back)).cloned() {
                    return Ok(Operand::Var(d));
                }
                if back == 1 {
                    let d = rot_dest
                        .get(&q)
                        .cloned()
                        .ok_or("需要轮转 phi 但生产者不在轮转集合里")?;
                    return Ok(Operand::Var(d));
                }
                if back >= 2 {
                    // 上面那个 `rot` 扫描已经拦过一轮；这里不是走过场，而是「谁都不能
                    // 默默拿本轮的值当上一轮」这条必须落在引用点上。
                    return Err("跳轮引用需要超过一级轮转寄存器");
                }
                if row_pos.get(&q).copied().unwrap_or(usize::MAX) >= row_pos[&p] {
                    return Err("kernel 内本轮生产者排在消费者之后");
                }
                Ok(Operand::Var(kernel_name[q].clone().ok_or("本轮生产者没有输出")?))
            },
        }
    };

    let build = |p: usize, k: Option<usize>| -> Result<Inst, &'static str> {
        let src = &lp.nodes[p];
        if !inst_is_materializable(&src.kind) {
            return Err("调度区内有不可按迭代复制的指令");
        }
        let f = |o: &Operand| resolve(o, p, k);
        let kind = rewrite_kind(&src.kind, &f)?;
        let dest = match (&src.dest, k) {
            (Some(_), Some(kk)) => Some(
                inst_name
                    .get(&(p, kk))
                    .cloned()
                    .ok_or("该实例没有被命名（切分与节点集不自洽）")?,
            ),
            (Some(_), None) => Some(kernel_name[p].clone().ok_or("kernel 实例没有被命名")?),
            (None, _) => None,
        };
        Ok(Inst {
            dest,
            kind,
            span: src.span.clone(),
        })
    };

    // 轮转格的播种：读「进循环前的值」那些排在 prologue **之前**，其余插在它读的
    // 那个实例之后。位置不是风格：实例名重建后就是基名槽，早一步晚一步拿到的是
    // 不同的份（与 `emitted_reads_match` 里的 `seed_cells` 同一条发射序）。
    let mut prologue_insts: Vec<Inst> = Vec::new();
    prologue_insts.extend(seed_first.iter().cloned());
    for &(p, k) in &plan.prologue {
        prologue_insts.push(build(p, Some(k))?);
        if let Some(seeds) = seed_after.remove(&(p, k)) {
            prologue_insts.extend(seeds);
        }
    }
    if !seed_after.is_empty() {
        // 格子要的那个实例根本不在 prologue 里 ⇒ 播种无处可插，拿旧值发上去就是错值。
        return Err("轮转格的播种实例不在 prologue 里");
    }
    let mut kernel_insts: Vec<Inst> = Vec::new();
    for &p in row {
        kernel_insts.push(build(p, None)?);
    }
    // 行尾拷贝链（lag 从大到小）：它就是「真轮转」在机器码层面的形（上一轮的值
    // 逐格后移），而 `emitted_reads_match` 的 `shift_cells` 按同一份序模拟。
    kernel_insts.extend(row_copies.iter().cloned());
    let mut epilogue_insts: Vec<Inst> = Vec::new();
    for &(p, k) in &plan.epilogue {
        epilogue_insts.push(build(p, Some(k))?);
    }

    // 4. 判定指令处理：确保循环正确轮次，不多跑一轮
    let mut cond_dest_kern: Option<SsaVar> = None;
    if let Some(d) = &lp.cond.dest {
        let v2 = alloc.fresh(&d.base);
        cond_dest_kern = Some(SsaVar {
            base: d.base.clone(),
            version: v2,
            ty: d.ty.clone(),
            span: d.span.clone(),
        });
    }
    // prologue 里每个节点最后一次输出的名字，供 preheader 的判定取「最新值」。
    // kernel 头部的判定：使用轮转 phi 的值（上一轮结果），决定是否继续循环
    let mut prologue_last: HashMap<usize, SsaVar> = HashMap::new();
    for &(p, k) in plan.prologue.iter() {
        if let Some(sv) = inst_name.get(&(p, k)) {
            prologue_last.insert(p, sv.clone());
        }
    }

    // kernel 判定：使用轮转 phi / 轮转格的值（上一轮结果），决定是否继续循环
    let mk_cond_kern = |dest: &Option<SsaVar>| -> Result<Inst, &'static str> {
        let f = |o: &Operand| -> Result<Operand, &'static str> {
            let Operand::Var(v) = o else {
                return Ok(o.clone());
            };
            let key = v.name();
            let q = lp
                .phi_sources
                .get(&key)
                .copied()
                .or_else(|| def_of.get(&key).copied());
            match q {
                Some(q) => {
                    let back = back_rounds(cs, shift[q], lp.phi_sources.contains_key(&key));
                    let d = cell_read.get(&(q, back)).cloned().or_else(|| {
                        if back == 1 {
                            rot_dest.get(&q).cloned()
                        } else {
                            None
                        }
                    });
                    Ok(d.map(Operand::Var).unwrap_or_else(|| o.clone()))
                },
                None => Ok(o.clone()),
            }
        };
        Ok(Inst {
            dest: dest.clone(),
            kind: rewrite_kind(&lp.cond.kind, &f)?,
            span: lp.cond.span.clone(),
        })
    };

    let cond_kern = mk_cond_kern(&cond_dest_kern)?;
    let cond_var_kern = cond_dest_kern.clone().ok_or("判定指令没有输出")?;
    // kernel 的 head 块名：由 body 名派生，多循环场景下也不会相撞。
    let khead = format!("{header}.khead");

    // 5. 一次性写回。
    // kernel 落成**两块**：head（轮转 phi + 判定 + 分支）与 body（按 T 序的全部调度
    // 节点 + 跳回 head）。单块自环的 kernel 实测会被结构化重建弄丢回边（-O2 下 `ch`
    // 只跑一轮：`cmp` 后的两条分支都跳向出口），而两块形式正是 SSA 构建器为 `while`
    // 产出的形状，重建认识它。
    {
        let pre = func.blocks.get_mut(&preheader).ok_or("preheader 不存在")?;
        pre.insts.extend(prologue_insts);
        pre.term = crate::ssa::Terminator::Jump(khead.clone());
    }
    {
        // body：轮转 phi 不在这里（它们在 head，每轮只赋一次上一轮的值）。
        let h = func.blocks.get_mut(&header).ok_or("头块不存在")?;
        h.phis = Vec::new();
        h.insts = kernel_insts;
        h.term = crate::ssa::Terminator::Jump(khead.clone());
    }
    func.blocks.insert(
        khead.clone(),
        BasicBlock {
            name: khead.clone(),
            phis: kernel_phis,
            insts: vec![cond_kern],
            term: crate::ssa::Terminator::Branch {
                cond: Operand::Var(cond_var_kern),
                t: header.clone(),
                f: exit.clone(),
            },
            preds: Vec::new(),
            succs: Vec::new(),
        },
    );
    // 块序里把 head 插在 body 之前：重建与诊断都按这个序遍历，必须确定性。
    match func.block_order.iter().position(|b| *b == header) {
        Some(pos) => func.block_order.insert(pos, khead.clone()),
        None => func.block_order.push(khead.clone()),
    }
    {
        let x = func.blocks.get_mut(&exit).ok_or("出口块不存在")?;
        let mut merged = epilogue_insts;
        merged.extend(x.insts.iter().cloned());
        x.insts = merged;
    }
    // 6. 区域外的引用修复。区域内节点的值被改名成各实例的新名字，而**区域外的指令
    //    也会读它们** —— 典型形态是出口块的 `return s` 读累加器，也包括任何
    //    terminator 里的操作数。第一次实现只改了指令侧、漏了 term 侧，于是留下
    //    `for.end: s.2` 这种悬空引用。规则：一律改指向该节点在 T 序上**最后执行的
    //    那个实例**（出循环时能看到的正是它最后一次赋值）。
    let mut last_of: HashMap<usize, SsaVar> = HashMap::new();
    for &(p, k) in plan.flatten().iter() {
        if lp.nodes[p].dest.is_some() {
            if let Some(sv) = inst_name.get(&(p, k)) {
                last_of.insert(p, sv.clone());
            }
        }
    }
    let reroute = |o: &Operand| -> Result<Operand, &'static str> {
        if let Operand::Var(v) = o {
            if let Some(&q) = def_of.get(&v.name()) {
                let sv = last_of
                    .get(&q)
                    .cloned()
                    .ok_or("区域外引用找不到对应的最后实例")?;
                return Ok(Operand::Var(sv));
            }
        }
        Ok(o.clone())
    };
    for name in crate::ssa::ordered_block_names(func) {
        if lp.region_bbs.contains(&name) {
            continue;
        }
        let (new_insts, new_phis, new_term) = {
            let bb = func.blocks.get(&name).ok_or("块不存在")?;
            let mut items: Vec<Inst> = Vec::with_capacity(bb.insts.len());
            for inst in &bb.insts {
                items.push(Inst {
                    dest: inst.dest.clone(),
                    kind: rewrite_kind(&inst.kind, &reroute)?,
                    span: inst.span.clone(),
                });
            }
            let phis: Vec<Phi> = bb
                .phis
                .iter()
                .map(|p| {
                    let mut inc = Vec::with_capacity(p.incoming.len());
                    for (s, f) in &p.incoming {
                        match reroute(&Operand::Var(s.clone()))? {
                            Operand::Var(x) => inc.push((x, f.clone())),
                            _ => return Err("phi 输入必须是变量"),
                        }
                    }
                    Ok(Phi {
                        dest: p.dest.clone(),
                        incoming: inc,
                    })
                })
                .collect::<Result<Vec<Phi>, &'static str>>()?;
            let term = match &bb.term {
                crate::ssa::Terminator::Ret { val } => crate::ssa::Terminator::Ret {
                    val: match val {
                        Some(v) => Some(reroute(v)?),
                        None => None,
                    },
                },
                crate::ssa::Terminator::Jump(t) => crate::ssa::Terminator::Jump(t.clone()),
                crate::ssa::Terminator::Branch { cond, t, f } => crate::ssa::Terminator::Branch {
                    cond: reroute(cond)?,
                    t: t.clone(),
                    f: f.clone(),
                },
                crate::ssa::Terminator::SwitchTable {
                    idx,
                    values,
                    targets,
                    default,
                } => crate::ssa::Terminator::SwitchTable {
                    idx: reroute(idx)?,
                    values: values.clone(),
                    targets: targets.clone(),
                    default: default.clone(),
                },
                crate::ssa::Terminator::Dispatch { targets } => {
                    crate::ssa::Terminator::Dispatch {
                        targets: targets.clone(),
                    }
                },
                crate::ssa::Terminator::Unreachable => crate::ssa::Terminator::Unreachable,
            };
            (items, phis, term)
        };
        let bb = func.blocks.get_mut(&name).ok_or("块不存在")?;
        bb.insts = new_insts;
        bb.phis = new_phis;
        bb.term = new_term;
    }
    // latch 块已成死块：确认没人指向它，然后从 CFG 摘除。
    for name in crate::ssa::ordered_block_names(func) {
        if name == latch || name == header {
            continue;
        }
        if succ_names(func, &name).iter().any(|s| *s == latch) {
            return Err("latch 仍被其它块引用，无法摘除");
        }
    }
    {
        // 头块自己的回边指向自己，是 kernel 的正常形状；其余块已核实无人指向 latch。
        func.blocks.remove(&latch);
        func.block_order.retain(|b| *b != latch);
    }
    Ok(())
}

// ================================================== 诊断窗口（DCC_PIPE_DUMP）

/// 把一个候选循环的**识别 / 调度 / 三段切分**打成文本。
///
/// 为什么必须有它：流水的错值只在「管线中途的 IR」上出现（教训：旧 P16 的接入点
/// 放在管线末尾时，开与关的 -O2 产物逐字节相同），而单测看的是未经其它 pass 的 SSA ——
/// 两边都对不上时，没有这个窗口就只能靠猜形状。
/// 由 `crate::ssa::passes::run_pipelining` 在 `DCC_PIPE_DUMP` 存在时调用，只写 stderr，
/// 不参与产物字节。
pub fn dump_plan(func: &Function, p: &PipelinePlan) -> String {
    let lp = &p.lp;
    let mut s = String::new();
    s.push_str(&format!(
        "fn {} header={} latch={} preheader={} exit={} region={:?} iv={} trip={}\n",
        func.name, lp.header, lp.latch, lp.preheader, lp.exit, lp.region_bbs, lp.iv_base, lp.trip
    ));
    for (i, nd) in lp.nodes.iter().enumerate() {
        let d = nd
            .dest
            .as_ref()
            .map(|x| x.name())
            .unwrap_or_else(|| "-".to_string());
        s.push_str(&format!("  node[{i}] {d} = {:?}\n", nd.kind));
    }
    let cd = lp
        .cond
        .dest
        .as_ref()
        .map(|x| x.name())
        .unwrap_or_else(|| "-".to_string());
    s.push_str(&format!("  cond    {cd} = {:?}\n", lp.cond.kind));
    let mut ps: Vec<(String, usize)> = lp
        .phi_sources
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    ps.sort();
    s.push_str(&format!("  phi_sources={ps:?}\n"));
    s.push_str(&format!("  ii={} stage={:?}\n", p.ii, p.stage));
    let inst = &p.inst;
    s.push_str(&format!(
        "  depth={} shift={:?} rounds={} row={:?}\n",
        inst.depth,
        inst.shift,
        inst.kernel.len(),
        inst.kernel.first()
    ));
    let names: Vec<String> = lp
        .nodes
        .iter()
        .map(|n| {
            n.dest
                .as_ref()
                .map(|x| x.name())
                .unwrap_or_else(|| "-".to_string())
        })
        .collect();
    let show = |v: &[(usize, usize)]| -> Vec<String> {
        v.iter()
            .map(|&(q, k)| {
                format!("{}#{k}[{}]", names.get(q).map(String::as_str).unwrap_or("?"), q)
            })
            .collect()
    };
    s.push_str(&format!("  prologue ={:?}\n", show(&inst.prologue)));
    s.push_str(&format!("  epilogue ={:?}\n", show(&inst.epilogue)));
    // 真轮转格（P16-6.3）：它就是把「同基名不能同时保留两份」的拒收换成 N 份独立位置，
    // 所以「接受了几个格、每个格是谁的哪一轮」必须直接可读（`DCC_PIPE_DUMP=2`）。
    if inst.cells.is_empty() {
        s.push_str("  rot_cells=none(基名塌缩)\n");
    } else {
        let cells: Vec<String> = inst
            .cells
            .iter()
            .map(|c| {
                format!(
                    "{}#{}:lag{}->{}",
                    names.get(c.q).map(String::as_str).unwrap_or("?"),
                    c.q,
                    c.lag,
                    c.home
                )
            })
            .collect();
        s.push_str(&format!("  rot_cells={cells:?}\n"));
    }
    s.push_str(&format!(
        "  verify={} flatten_len={}\n",
        inst.verify(&p.stage),
        inst.flatten().len()
    ));
    s
}

#[cfg(test)]
mod apply_tests {
    use super::*;
    use crate::ssa::Function;

    // 同样用依赖链作为可流水目标（理由见 materialize_tests）。
    const RED: &str = "int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}\n";

    fn red_fn() -> Function {
        let (prog, _d) = crate::parser::parse(RED.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        for decl in m.source().decls.iter() {
            if let crate::ast::Decl::Func(f) = decl {
                if f.name.name == "ch" {
                    return crate::ssa::build_from_func(f).expect("应能 SSA 化");
                }
            }
        }
        panic!("找不到 ch");
    }

    fn snapshot(f: &Function) -> String {
        let mut parts = Vec::new();
        for name in crate::ssa::ordered_block_names(f) {
            let bb = &f.blocks[&name];
            parts.push(format!(
                "{}|phis={}|insts={}{}",
                name,
                bb.phis.len(),
                bb.insts.len(),
                bb.insts
                    .iter()
                    .map(|i| format!("#{}", i.dest.as_ref().map(|d| d.name()).unwrap_or_default()))
                    .collect::<String>()
            ));
        }
        parts.join(";")
    }

    /// 落地后的 IR 必须仍是合法 SSA：无悬空引用、单赋值、kernel 是单块自然循环，
    /// 而且必须能被结构化重建回 AST（rebuild 支持这个形状才算真能落地）。
    #[test]
    fn apply_produces_valid_rebuildable_ir() {
        let mut f = red_fn();
        let lp = classify_loop(&f, "for.body").expect("red 应被识别");
        let p = plan_for_header_with(&f, "for.body", &Resources::default()).expect("red 应过闸门");
        apply_plan(&mut f, &lp, &p.inst).expect("落地应成功");

        assert!(crate::ssa::dangling_refs(&f).is_empty(), "落地后不得有悬空引用");
        assert!(crate::ssa::duplicate_ssa_defs(&f).is_empty(), "落地后必须保持单赋值");

        // kernel：单块、有轮转 phi、含全部节点再加一末尾判定，回边指向自己、出口唯一。
        let h = &f.blocks["for.body"];
        assert_eq!(
            h.insts.len(),
            lp.nodes.len(),
            "body = 全部调度节点；判定已移到 head（单块自环会被重建丢回边）"
        );
        assert!(h.phis.is_empty(), "轮转 phi 属于 head，不该留在 body");
        assert!(
            matches!(&h.term, crate::ssa::Terminator::Jump(t) if t == "for.body.khead"),
            "body 必须跳回 head"
        );
        let head = &f.blocks["for.body.khead"];
        assert_eq!(head.insts.len(), 1, "head 只含那一份判定指令");
        assert!(!head.phis.is_empty(), "跳轮引用必须在 head 上生成轮转 phi");
        match &head.term {
            crate::ssa::Terminator::Branch { t, f: fx, .. } => {
                assert_eq!(t, "for.body");
                assert_eq!(fx, "for.end");
            },
            _ => panic!("head 必须是条件分支：环内进 body，环外出到 for.end"),
        }
        // head 必须排在 body 之前：重建与诊断都按块序遍历，这个序必须确定。
        let ord = f.block_order.clone();
        let pos = |k: &str| ord.iter().position(|b| b == k).unwrap_or(usize::MAX);
        assert!(pos("for.body.khead") < pos("for.body"), "head 必须排在 body 之前");
        // latch 块已被摘除，且不在块序里。
        assert!(!f.blocks.contains_key("for.step"), "latch 块应被摘除");
        assert!(!f.block_order.iter().any(|b| b == "for.step"));
        // preheader 无条件跳转：否则首次进 kernel 无人判定，
        // 整个循环变成 do-while 语义、至少多跑一轮（就是那次越界写）。
        // 出口块前置 epilogue。
        assert!(
            matches!(&f.blocks["for.cond"].term, crate::ssa::Terminator::Jump(t) if t == "for.body.khead"),
            "preheader 只能无条件进 head（判定统一由 head 负责）"
        );
        assert!(
            f.blocks["for.cond"].insts.len() >= 1 + p.inst.prologue.len(),
            "preheader = 原有指令 + prologue"
        );
        assert!(f.blocks["for.end"].insts.len() >= p.inst.epilogue.len());

        // 结构化重建必须跑通（这才是「能进产物」的前提）。
        let ast = crate::ssa::rebuild_block(&f);
        assert!(!ast.items.is_empty(), "重建结果不能是空块");
    }

    /// 拿旧计划改新 IR 必须被拒 —— 否则会得到自洽但错误的代码。
    #[test]
    fn apply_refuses_stale_plan() {
        let mut f = red_fn();
        let lp = classify_loop(&f, "for.body").expect("red 应被识别");
        let p = plan_for_header(&f, "for.body").expect("red 应过闸门");
        // 把区域里一条指令换成别的形态，模拟「IR 已被其它 pass 改动」。
        let mut stale = lp.clone();
        if let Some(d) = stale.nodes.get(1).and_then(|i| i.dest.clone()) {
            stale.nodes[1] = Inst {
                dest: Some(d),
                kind: InstKind::Bin {
                    op: BinOp::Mul,
                    l: Operand::Int(3),
                    r: Operand::Int(3),
                },
                span: stale.nodes[1].span.clone(),
            };
        }
        let before = snapshot(&f);
        let e = apply_plan(&mut f, &stale, &p.inst).expect_err("过期计划必须被拒");
        assert!(e.contains("不匹配"), "应报计划与 IR 不匹配，实得：{e}");
        assert_eq!(snapshot(&f), before, "拒绝时不得改动原 IR");
    }

    /// 失败安全：落地中途失败必须原样返回，不留半改的 IR。
    /// 这是「先在副本上改、全过才替换」这条设计的直接验证。
    #[test]
    fn apply_failure_leaves_ir_untouched() {
        let mut f = red_fn();
        let lp = classify_loop(&f, "for.body").expect("red 应被识别");
        let p = plan_for_header(&f, "for.body").expect("red 应过闸门");
        let mut bad = p.inst.clone();
        // 造一个迭代号越界的实例：切分与 trip 不自洽。
        bad.epilogue.push((0, 999));
        let before = snapshot(&f);
        let e = apply_plan(&mut f, &lp, &bad).expect_err("越界实例必须被拒");
        assert!(e.contains("超出 trip 范围") || e.contains("覆盖不完整"), "实得：{e}");
        assert_eq!(snapshot(&f), before, "落地失败时原 IR 必须逐字不变");
    }
}
