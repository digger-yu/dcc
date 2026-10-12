// src/ssa/liveness.rs —— 逐点活性与干涉表（P12 Phase 5.1 的数据地基）。
//
// 为什么要有它：`docs/p12-physreg-spill-design.md` §3 的 H2 说得很具体 —— 「把
// caller-saved 纳进来需要的是一张真正的干涉表（哪个值在哪段代码上活到哪个号上）」，
// 而 P12-A 今天只有基名的 `[最早, 最晚]` 粗区间，且那个区间**只用来排序**，不参与
// 「能不能共用一个号」的判断（`regalloc::plan_for` 里那条「一个号只给一个变量」的
// 实测判据）。没有干涉数据，溢出决策与寄存器共用都只能靠猜 —— §7.20 记的那条
// `char16_t/char32_t` 共用 %rbx 导致 -O2 rc=3 的错值，正是「用粗区间当干涉」的代价。
//
// 本模块只做**事实**（谁和谁同时活着），不做决策：分配策略、代价模型、spill 选择都在
// `regalloc.rs`。层次因此是清楚的：这里回答「活着吗」，那里回答「给谁」。
//
// 不做的事（设计 §1 非目标）：图着色、分裂合并、coalescing。干涉图是它们的输入，
// 不是它们本身。

use crate::ssa::cfg::{ordered_block_names, term_succs_pub};
use crate::ssa::ir::{collect_uses_kind, Function, Operand, Terminator};
use std::collections::{BTreeSet, HashMap};

/// 块内的一个程序点：一条指令（或 phi 段、terminator）。
///
/// 必须把**非定义指令**也当成点：它的使用会延后名字的活性。只遍历定义点的写法会
/// 漏掉「先定义、后面才被用」这种情况，算出来的活性范围偏短 —— 偏短的干涉会允许
/// 两个真同时活着的值共用一个号，那是静默错值那一类。
struct Point {
    /// 本点读到的 SSA 名
    uses: BTreeSet<String>,
    /// 本点写的 SSA 名（SSA 单定义 ⇒ 至多一个）
    def: Option<String>,
}

/// 干涉表：`ssa 名 → 与它同时活着的 SSA 名`（对称按构造保证）。
#[derive(Debug, Clone, Default)]
pub struct Interference {
    conflicts: HashMap<String, BTreeSet<String>>,
}

impl Interference {
    pub fn conflicts_of(&self, ssa_name: &str) -> &BTreeSet<String> {
        static EMPTY: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
        self.conflicts
            .get(ssa_name)
            .unwrap_or(EMPTY.get_or_init(BTreeSet::new))
    }

    /// 两个 SSA 名是否同时活着。`a == b` 返回 true（自己与自己当然共用不了两个号）。
    pub fn interferes(&self, a: &str, b: &str) -> bool {
        a == b || self.conflicts_of(a).contains(b)
    }

    /// 两个基名之间是否存在**一对不冲突的版本**。这是个**弱**判据：它只回答「两者能否
    /// 在时间上错开」，**不能**用来判定「两者可以共用一个号」（那要下面的
    /// `ranges_disjoint`）。混用会高估收益 —— 本仓在第一版归属度量上就是这么错的
    /// （见设计 §11.9 的订正）。它仍然有用：回答「同基名的不同版本能不能拿到不同的家」，
    /// 那件事确实只需一对不重叠的版本 + 分裂成两个真局部。
    pub fn bases_have_disjoint_pair(&self, a: &[String], b: &[String]) -> bool {
        a.iter().any(|x| b.iter().any(|y| !self.interferes(x, y)))
    }

    /// **共用一个号的真正条件**：两边的全部版本两两都不干涉（∀，不是 ∃）。
    ///
    /// 为什么必须全量：一个基名在重建后是**一个**格子（家对它是全程唯一的），所以只要
    /// 它任一版本与对方任一版本同时活着，共用就会覆盖其中一个 —— 那正是 §7.20 实测过的
    /// `c16`/`c32` 共用 %rbx 导致 -O2 rc=3 的形状。
    pub fn ranges_disjoint(&self, a: &[String], b: &[String]) -> bool {
        a.iter().all(|x| b.iter().all(|y| !self.interferes(x, y)))
    }
}

fn operand_vars(o: &Operand) -> Vec<String> {
    match o {
        Operand::Var(v) => vec![v.name()],
        Operand::Phi(pairs) => pairs.iter().map(|(v, _)| v.name()).collect(),
        _ => Vec::new(),
    }
}

fn term_point(t: &Terminator) -> Point {
    let mut uses = BTreeSet::new();
    match t {
        Terminator::Ret { val: Some(o) } => {
            for v in operand_vars(o) {
                uses.insert(v);
            }
        },
        Terminator::Branch { cond, .. } | Terminator::SwitchTable { idx: cond, .. } => {
            for v in operand_vars(cond) {
                uses.insert(v);
            }
        },
        Terminator::Ret { val: None } | Terminator::Jump(_) | Terminator::Unreachable => {},
    }
    Point { uses, def: None }
}

/// 算出整个函数的干涉表。
///
/// 两块标准算法：
/// 1. 先按块收集 `gen`（本块在用、且在本块之前没有被重定义）与 `kill`（本块定义的），
///    再在 CFG 上反向迭代到不动点得到每块的 `live_after`；
/// 2. 块内**逆序**逐点维护「这点之后还活着什么」，得到每个定义点的冲突集。
///
/// SSA 单定义 ⇒ 「本块定义了的名字不会是 live-in」是成立的（定义支配使用），所以
/// `gen - kill` 这一步不会少算活性。保守方向是**多算冲突**：多算只会少共用（少绑），
/// 少算会撞家（静默错值）。
pub fn compute(func: &Function) -> Interference {
    let order = ordered_block_names(func);
    let idx: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let n = order.len();

    let mut points: Vec<Vec<Point>> = (0..n).map(|_| Vec::new()).collect();
    let mut gens: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
    let mut kills: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];

    for (bi, bname) in order.iter().enumerate() {
        let Some(bb) = func.blocks.get(bname) else {
            continue;
        };
        // phi 在块首：incoming 是「进入本块时」的读，dest 是本块的写
        let mut phi_uses: BTreeSet<String> = BTreeSet::new();
        for phi in &bb.phis {
            for (src, _) in &phi.incoming {
                phi_uses.insert(src.name());
            }
            points[bi].push(Point {
                uses: phi_uses.clone(),
                def: Some(phi.dest.name()),
            });
            kills[bi].insert(phi.dest.name());
        }
        for inst in &bb.insts {
            let mut uses: BTreeSet<String> = BTreeSet::new();
            for u in collect_uses_kind(&inst.kind) {
                uses.insert(u);
            }
            let def = inst.dest.as_ref().map(|d| d.name());
            for u in uses.iter() {
                gens[bi].insert(u.clone());
            }
            if let Some(d) = &def {
                kills[bi].insert(d.clone());
            }
            points[bi].push(Point { uses, def });
        }
        let tp = term_point(&bb.term);
        for u in &tp.uses {
            gens[bi].insert(u.clone());
        }
        points[bi].push(tp);
        // gen 刨掉本块 kill 的名字（SSA 下成立，见函数头）
        let k = kills[bi].clone();
        for x in k {
            gens[bi].remove(&x);
        }
    }

    let succs: Vec<Vec<usize>> = order
        .iter()
        .map(|bname| match func.blocks.get(bname) {
            Some(bb) => term_succs_pub(&bb.term)
                .iter()
                .filter_map(|s| idx.get(s.as_str()).copied())
                .collect(),
            None => Vec::new(),
        })
        .collect();

    let mut live_after: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for bi in 0..n {
            let mut next: BTreeSet<String> = BTreeSet::new();
            for &si in &succs[bi] {
                for g in gens[si].iter() {
                    next.insert(g.clone());
                }
                for l in live_after[si].iter() {
                    if !kills[si].contains(l) {
                        next.insert(l.clone());
                    }
                }
            }
            if next != live_after[bi] {
                live_after[bi] = next;
                changed = true;
            }
        }
    }

    let mut out = Interference::default();
    for bi in 0..n {
        let mut live = live_after[bi].clone();
        for pt in points[bi].iter().rev() {
            if let Some(def) = &pt.def {
                // 定义点的冲突集 = 这之后还活着的 + 本点自己读的（读完才死）
                let mut cf = live.clone();
                for u in &pt.uses {
                    cf.insert(u.clone());
                }
                cf.remove(def);
                for o in cf.iter() {
                    out.conflicts
                        .entry(def.clone())
                        .or_default()
                        .insert(o.clone());
                    out.conflicts
                        .entry(o.clone())
                        .or_default()
                        .insert(def.clone());
                }
            }
            // 走到这点之前：读让它活起来，写让它在此之前不存在
            for u in &pt.uses {
                live.insert(u.clone());
            }
            if let Some(def) = &pt.def {
                live.remove(def);
            }
        }
    }
    out
}

/// 某个基名的全部 SSA 名（用于基名之间的干涉判定）。
pub fn versions_of_base(func: &Function, base: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for bb in func.blocks.values() {
        for phi in &bb.phis {
            if phi.dest.base == base {
                out.push(phi.dest.name());
            }
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                if d.base == base {
                    out.push(d.name());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 某个基名的活区间是否**跨过一次 call**（Phase 5.1 前置三的收益面度量）。
///
/// 只用于度量与用例，不进任何分配决策：它回答的是「如果允许把 caller-saved 号发当家，
/// 需要保存通道的占多少、不需要的占多少」。口径与 `compute` 一致：同一个
/// `ordered_block_names` 顺序、同一套 `collect_uses_kind`，因此不会出现「度量用的活性
/// 与发号用的活性不是一套」这种第二真源。
///
/// 保守方向是**多报跨 call**（区间 [首次定义, 最后使用] 内出现任何一个 call 就算跨），
/// 因为少报的后果是给真的跨 call 的值发一个会被毁掉的号。
pub fn spans_call(func: &Function, base: &str) -> bool {
    let versions = versions_of_base(func, base);
    if versions.is_empty() {
        return false;
    }
    let order = ordered_block_names(func);
    let mut seq: usize = 0usize;
    let mut first_def: Option<usize> = None;
    let mut last_use: Option<usize> = None;
    // call 点先收集、**末尾再判区间**：在扫到 call 那一刻，“最后使用”往往还没出现
    // （`int a = sink(v); return v + a;` 里 v 在 call 之后才被读），当场判会漏报。
    // 本用例集里 `spans_call_is_true_for_a_range_that_straddles_a_call` 钉的就是这个形状。
    let mut call_points: Vec<usize> = Vec::new();
    for bname in &order {
        let Some(bb) = func.blocks.get(bname) else {
            continue;
        };
        // phi：incoming 是读、dest 是写，都算在本点
        for phi in &bb.phis {
            seq += 1;
            let reads = phi.incoming.iter().any(|(v, _)| v.base == base);
            let writes = phi.dest.base == base;
            if writes {
                first_def.get_or_insert(seq);
            }
            if reads {
                last_use = Some(seq);
            }
        }
        for inst in &bb.insts {
            seq += 1;
            let is_call = matches!(
                inst.kind,
                crate::ssa::InstKind::Call { .. } | crate::ssa::InstKind::Invoke { .. }
            );
            let reads = collect_uses_kind(&inst.kind)
                .iter()
                .any(|v| versions.contains(v));
            let writes = inst.dest.as_ref().map(|d| d.base == base).unwrap_or(false);
            if writes {
                first_def.get_or_insert(seq);
            }
            if reads {
                last_use = Some(seq);
            }
            if is_call {
                call_points.push(seq);
            }
        }
        let tp_uses_base = match &bb.term {
            Terminator::Ret { val: Some(o) }
            | Terminator::Branch { cond: o, .. }
            | Terminator::SwitchTable { idx: o, .. } => {
                operand_vars(o).iter().any(|v| versions.contains(v))
            },
            _ => false,
        };
        seq += 1;
        if tp_uses_base {
            last_use = Some(seq);
        }
    }
    match (first_def, last_use) {
        (Some(a), Some(b)) => call_points.iter().any(|c| *c >= a && *c <= b),
        // 只有定义没有使用（或反之）的基名：保守报 true，因为下一轮可能就在区间内
        // 毁掉它；少报的代价是给了一个会被毁的号。
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssa_of(src: &str, fname: &str) -> Function {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let fd = m
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                crate::ast::Decl::Func(f) if f.name.name == fname => Some(f.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("源里没有函数 {fname}"));
        let mut func = crate::ssa::build_from_func(&fd)
            .unwrap_or_else(|| panic!("{fname} 应能 SSA 化，否则测不到干涉"));
        crate::ssa::passes::run_with_opts(&mut func, 2, true);
        func
    }

    /// 反向对照（不能只会说「都冲突」）：热循环里的累加器与归纳变量跨块同时活着，
    /// 必须判成互相干涉 —— 它们今天各自占一个 callee-saved，共用即错值。
    #[test]
    fn loop_carried_values_interfere() {
        let func =
            ssa_of("long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}", "s");
        let iv = compute(&func);
        let acc = versions_of_base(&func, "acc");
        let i = versions_of_base(&func, "i");
        assert!(!acc.is_empty() && !i.is_empty(), "被测源应有 acc 与 i 的版本");
        // 携带到下一代的 acc 版本（在循环体里被重新定义的那些）必须与某个 i 同时活着。
        // 不要求「任意两版本都冲突」：活区长度本来就不同，那样断言是错的（本轮踩过）。
        let carried = acc
            .iter()
            .filter(|a| iv.conflicts_of(a).iter().any(|o| i.iter().any(|x| x == o)))
            .count();
        assert!(
            carried >= 2,
            "至少两个 acc 版本应与某个 i 版本干涉，实得 {carried}；acc={acc:?} i={i:?}"
        );
        // 且不得把一切都说成互相冲突，否则这张表与今天的 blanket 规则等价，白做
        let all: Vec<&String> = acc.iter().chain(i.iter()).collect();
        let mut conflicts = 0;
        for x in &all {
            for y in &all {
                if x < y && iv.interferes(x, y) {
                    conflicts += 1;
                }
            }
        }
        let pairs = all.len() * all.len().saturating_sub(1) / 2;
        assert!(
            pairs > 0 && conflicts < pairs,
            "干涉图不应是全连接：conflicts={conflicts} pairs={pairs}"
        );
    }

    /// 最强的结构不变式：**同一条指令的 def 与它的每一个 use 必须互为冲突**；
    /// phi 的 dest 与它的 incoming 也一样。漏一条就是「两个同时活着的值被认为可共用
    /// 一个号」—— §7.20 那条 `c16`/`c32` 共用 %rbx 导致 -O2 rc=3 正是这个形状。
    /// 在多份真实源上跑：它偷不了，任何一块漏算都会被抓到。
    #[test]
    fn def_and_its_uses_always_conflict() {
        for (src, fname) in [
            ("long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}", "s"),
            ("long f(long a,long b){long t=a+1;long u=b+t;long v=t*u;return v+t;}", "f"),
            ("long g(long n){long r=1;while(n>1){r=r*n;n=n-1;}return r;}", "g"),
            (
                "long h(long n){long x=0;for(long i=0;i<n;i++){long y=i*2;x=x+y;}return x;}",
                "h",
            ),
        ] {
            let func = ssa_of(src, fname);
            let iv = compute(&func);
            for bb in func.blocks.values() {
                for inst in &bb.insts {
                    let Some(d) = &inst.dest else { continue };
                    let dn = d.name();
                    for u in collect_uses_kind(&inst.kind) {
                        assert!(
                            iv.interferes(&dn, &u),
                            "{fname}：{dn} 的定义点读到 {u}，却不判干涉（活性漏算）"
                        );
                    }
                }
                for phi in &bb.phis {
                    let dn = phi.dest.name();
                    for (sv, _) in &phi.incoming {
                        assert!(
                            iv.interferes(&dn, &sv.name()),
                            "{fname}：phi {dn} 与 incoming {} 不判干涉",
                            sv.name()
                        );
                    }
                }
            }
        }
    }

    /// 判据的另一半：**不**同时活着的值必须判成不冲突。
    /// 用未优化的 IR（`build_from_func` 后不跑 pass）：跑了 -O2 会把整个函数常量折叠到
    /// 根本没有版本可测（本轮第一轮就踩了这个空跑坑）。
    #[test]
    fn sequential_disjoint_values_do_not_interfere() {
        let src =
            "long f(void){long a=1;long b=a+1;long c=b+b;long d=7;long e=d+d;long g=e+c;return g;}";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let fd = m
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                crate::ast::Decl::Func(f) if f.name.name == "f" => Some(f.clone()),
                _ => None,
            })
            .expect("f 应在源里");
        let func = crate::ssa::build_from_func(&fd).expect("f 应能 SSA 化");
        let iv = compute(&func);
        let a = versions_of_base(&func, "a");
        let d = versions_of_base(&func, "d");
        assert!(
            !a.is_empty() && !d.is_empty(),
            "未优化 IR 应保留 a 与 d 的版本（否则本用例是空跑）"
        );
        assert!(
            iv.bases_have_disjoint_pair(&a, &d),
            "a 在 d 定义之前就死了，两者应有一对不冲突的版本（a={a:?} d={d:?}）"
        );
    }

    /// §7.20 那条实测错值的形状（`char16_t c16` / `char32_t c32`）：干涉表必须能
    /// 认出它们**不同时活着**。今天 P12-A 靠「一个号只给一个变量」的 blanket 规则
    /// 回避了这个判断；Phase 5 要放开规则，判据就得先在这里立住。
    #[test]
    fn short_lived_char_pairs_are_detected_as_disjoint() {
        let func = ssa_of(
            "long f(void){char16_t c16='A';long x=c16;char32_t c32='B';long y=c32;return x+y;}",
            "f",
        );
        let iv = compute(&func);
        let a = versions_of_base(&func, "c16");
        let b = versions_of_base(&func, "c32");
        assert!(!a.is_empty() && !b.is_empty(), "源里应有 c16/c32（否则用例是空跑）");
        assert!(
            iv.bases_have_disjoint_pair(&a, &b),
            "c16 与 c32 的使用不相接，必须能找到不冲突的一对版本"
        );
    }

    /// 钉住 ∃ 与 ∀ 的区别（防止后人把 `ranges_disjoint` “修”回弱判据）：
    /// 热循环里 `acc` 与 `i` 存在不重叠的版本对（∃ 成立），但它们的活区整体相交，
    /// 所以 `ranges_disjoint` 必须为 false —— 共用一个号会覆盖其中一代的值。
    #[test]
    fn exists_disjoint_pair_is_not_the_same_as_disjoint_ranges() {
        let func =
            ssa_of("long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}", "s");
        let iv = compute(&func);
        let acc = versions_of_base(&func, "acc");
        let i = versions_of_base(&func, "i");
        assert!(
            iv.bases_have_disjoint_pair(&acc, &i),
            "循环里应有不重叠的版本对（否则两个判据没区别，用例就是空跑）"
        );
        assert!(!iv.ranges_disjoint(&acc, &i), "acc 与 i 的活区整体相交，不得判为可共用一个号");
    }

    /// 干涉关系必须**对称**：单向记账会让 `interferes(b,a)` 漏判，而漏判的后果就是两个
    /// 同时活着的值共用一个号。逐边反向检查，缺一条就把边名报出来。
    #[test]
    fn interference_is_symmetric() {
        let func =
            ssa_of("long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i*2;return acc;}", "s");
        let iv = compute(&func);
        let mut broken = Vec::new();
        for (name, set) in &iv.conflicts {
            for other in set {
                if !iv.conflicts_of(other).contains(name) {
                    broken.push(format!("{name} -> {other}"));
                }
            }
        }
        assert!(broken.is_empty(), "干涉不对称的边: {broken:?}");
    }

    /// `spans_call` 的「跨」必须是真跨：变量定义在 call 前、最后使用在 call 后。
    /// 这个度量直接决定前置三的收益面，算错就会把设计带倒。
    ///
    /// 注意形状：`v` 的定义**故意不取常量** —— `int v = 42` 会被 GVN 折掉，SSA 里根本
    /// 没有 `v` 的版本（下一跳用例把这个行为钉住：无版本 ⇒ 报 false，因为没有任何地方
    /// 会把值放在一个会被毁的号里）。
    #[test]
    fn spans_call_is_true_for_a_range_that_straddles_a_call() {
        let func = ssa_of(
            "int sink(int); int q(int n){int v = n + 1; int a = sink(v); return v + a;}",
            "q",
        );
        assert!(
            !versions_of_base(&func, "v").is_empty(),
            "夹具本身得先成立：v 在 SSA 里真有版本，否则下面的断言是在测「不存在」"
        );
        assert!(
            spans_call(&func, "v"),
            "v 定义在 call 前、又在 call 后被读 ⇒ 活区间必跨过那个 call"
        );
        // 常量被折叠：没有版本 ⇒ 不报跨（也不崩）。
        let folded = ssa_of("int sink(int); int q(void){int v = 42; return sink(v);}", "q");
        assert!(!spans_call(&folded, "v"), "无版本时应报 false");
    }

    /// 不跨 call 的两个形状都必须报 false：「先算完再用」与「根本不参与 call」。
    #[test]
    fn spans_call_is_false_when_the_range_ends_before_the_call() {
        let f1 =
            ssa_of("int sink(int); int q(void){int v = 42; int a = v + 1; return sink(a);}", "q");
        assert!(!spans_call(&f1, "v"), "v 的最后使用在 call 之前 ⇒ 不需要保存通道");
        let f2 = ssa_of("int q(void){int v = 42; return v + 1;}", "q");
        assert!(!spans_call(&f2, "v"), "函数里一个 call 都没有");
        // 反向对照：基名不存在时必须报 false而不是崩，也不能报成 true
        assert!(!spans_call(&f2, "不存在"), "不存在的基名不得被判为跨 call");
    }
}
