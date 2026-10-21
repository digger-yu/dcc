// src/ssa/cfg.rs - CFG 与支配关系 (SSA pass 基础设施)
//
// 放什么: 从 Terminator 派生 CFG 的前驱/后继 (term_succs / term_succs_pub /
//   collect_all_preds / refresh_cfg)、支配与后支配 (compute_dominators / compute_ipdoms /
//   intersect_post_dom / is_back_edge / is_cycle_entry / reverse_postorder / dfs_rpo)、
//   自然循环与确定遍历序 (find_loop_body{,_multi} / predecessor_counts / ordered_block_names)、
//   结构不变式判据 (dangling_refs / duplicate_ssa_defs)。
// 不放什么: 合法性判据以外的任何 IR 改写。

use std::collections::{HashMap, HashSet};

use crate::ssa::ir::{collect_uses_kind, collect_uses_operand, Function, Terminator};

/// 计算 BB 的 dominator set (经典迭代算法)。
/// Derive successor block names from a BB's terminator.
/// `bb.succs` can become stale after SSA passes mutate terminators
/// without updating it, so every analysis that needs successors must
/// derive them from the Terminator directly.
pub(super) fn term_succs(term: &Terminator) -> Vec<&str> {
    match term {
        Terminator::Jump(t) => vec![t.as_str()],
        Terminator::Branch { t, f, .. } => vec![t.as_str(), f.as_str()],
        Terminator::SwitchTable {
            targets, default, ..
        } => {
            let mut s: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
            if !s.contains(&default.as_str()) {
                s.push(default.as_str());
            }
            s
        },
        // computed goto: 目标集合就是全部 label BB（构造期已定）。
        Terminator::Dispatch { targets } => targets.iter().map(|t| t.as_str()).collect(),
        _ => vec![],
    }
}

/// Derive predecessor block names from all BB terminators.
/// Same rationale as `term_succs`: `bb.preds` can become stale.
///
/// 已 pub：P16 的循环识别同样只能从终结符派生前驱 —— `bb.preds` 是构造期缓存，改过
/// terminator 即脏，另写一份派生逻辑必然与这里漂移。
pub(super) fn collect_all_preds(func: &Function) -> HashMap<String, Vec<String>> {
    let mut preds: HashMap<String, Vec<String>> = HashMap::new();
    // 遍历序必须确定。直接 `for (name, bb) in &func.blocks` 迭代的是 HashMap,
    // 进程随机哈希种子会让同一个目标块的前驱列表每次给出不同顺序 —— 集合
    // 内容不变, 但这个序会随 `refresh_cfg` 写进 `bb.preds` 缓存, 于是
    // `DCC_SSA_IR_DUMP` 的读数逐次不稳定 (实测同一二进制连跑 8 次出两种序),
    // 拿它做差分对拍时会把「同一个二进制」误当成「两个行为」。
    // 走本模块的 `ordered_block_names`: 与 `passes::preds_from_terminators`
    // 用同一套确定序, 也让任何依赖前驱序的消费者天然可复现。
    for name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&name) else {
            continue;
        };
        for t in term_succs(&bb.term) {
            preds.entry(t.to_string()).or_default().push(name.clone());
        }
    }
    preds
}

pub(super) fn compute_dominators(func: &Function) -> HashMap<String, HashSet<String>> {
    let mut doms: HashMap<String, HashSet<String>> = HashMap::new();
    if !func.blocks.contains_key(&func.entry) {
        return doms;
    }
    // Derive preds from terminators to avoid stale bb.preds
    let derived_preds = collect_all_preds(func);
    // reverse postorder (now correctly derived from terminators)
    let rpo = reverse_postorder(func);
    let all: HashSet<String> = func.blocks.keys().cloned().collect();
    for b in func.blocks.keys() {
        doms.insert(b.clone(), all.clone());
    }
    doms.insert(func.entry.clone(), {
        let mut s = HashSet::new();
        s.insert(func.entry.clone());
        s
    });
    let mut changed = true;
    while changed {
        changed = false;
        for b in &rpo {
            if *b == func.entry {
                continue;
            }
            let preds = match derived_preds.get(b) {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };
            // intersect of preds
            let mut new: HashSet<String> = all.clone();
            for p in preds {
                if let Some(s) = doms.get(p) {
                    new = new.intersection(s).cloned().collect();
                }
            }
            new.insert(b.clone());
            if doms.get(b) != Some(&new) {
                doms.insert(b.clone(), new);
                changed = true;
            }
        }
    }
    // 不可达块 (CFG 有断边 / 前端漏连) 不在 rpo 里, 因而永远保留上面的 ⊤ 初值
    // = 「被所有块支配」。这会让它每条出边都被 `is_back_edge` 判成回边, 后端
    // SSA→AST 重建于是凭空造出 `while(1) {}` (运行期死循环)。支配关系只对从
    // entry 可达的块有定义: 定点迭代结束后把不可达块收敛成「只支配自己」。
    // 必须在定点循环**之后**做: 循环里 ⊤ 是交集的中性元素, 提前收紧会削掉
    // 可达块真实的支配集。
    let rpo_set: HashSet<&str> = rpo.iter().map(|s| s.as_str()).collect();
    for b in func.blocks.keys() {
        if !rpo_set.contains(b.as_str()) {
            let mut s = HashSet::new();
            s.insert(b.clone());
            doms.insert(b.clone(), s);
        }
    }
    doms
}

fn reverse_postorder(func: &Function) -> Vec<String> {
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    dfs_rpo(&func.entry, func, &mut visited, &mut order);
    order.reverse();
    order
}

fn dfs_rpo(bb: &str, func: &Function, visited: &mut HashSet<String>, order: &mut Vec<String>) {
    if !visited.insert(bb.to_string()) {
        return;
    }
    let succs = term_succs(&func.blocks[bb].term);
    for s in succs {
        dfs_rpo(s, func, visited, order);
    }
    order.push(bb.to_string());
}

/// 结构不变式检查: SSA IR 里每个被引用的名字都必须有定义。
///
/// 为什么需要一个「自己检查自己」的函数: 删除指令的 pass (值编号、死代码消除) 都是
/// 「改写引用 + 删除定义」两步动作, 其中任一步因guards/去重而被跳过时, 留下的就是
/// 指向已删定义的悬空引用 —— 重建后表现为读一个从未赋值的临时名/槽位, 是**静默**错值。
/// 本轮 GVN 就因「拒绝第二次改写同一源名但仍记录删除」造出过 `if (_tbin_2)` 这种引用。
/// 把它做成可调用的谓词, 单测就能把该不变式长期钉住。
pub(super) fn dangling_refs(func: &Function) -> Vec<String> {
    let mut defined: HashSet<String> = HashSet::new();
    for p in &func.params {
        defined.insert(p.name());
    }
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                defined.insert(d.name());
            }
        }
        for phi in &bb.phis {
            defined.insert(phi.dest.name());
        }
    }
    let mut bad: Vec<String> = Vec::new();
    for bb in ordered_block_names(func) {
        let Some(b) = func.blocks.get(&bb) else {
            continue;
        };
        let mut names: Vec<String> = Vec::new();
        for inst in &b.insts {
            names.extend(collect_uses_kind(&inst.kind));
        }
        for phi in &b.phis {
            for (v, _) in &phi.incoming {
                names.push(v.name());
            }
        }
        match &b.term {
            Terminator::Ret { val: Some(o) } => names.extend(collect_uses_operand(o)),
            Terminator::Branch { cond, .. } => names.extend(collect_uses_operand(cond)),
            Terminator::SwitchTable { idx, .. } => names.extend(collect_uses_operand(idx)),
            _ => {},
        }
        for n in names {
            if !defined.contains(&n) {
                bad.push(format!("{bb}: {n}"));
            }
        }
    }
    bad.sort();
    bad.dedup();
    bad
}

/// 结构不变式：全函数**单赋值** —— 每个 SSA 名最多被定义一次
/// (一个 dest 指令或一个 phi)。返回违反该不变式的名字列表。
///
/// 为什么必须有这条检查而不只靠 `dangling_refs`：pass 可以保留原有指令不动、
/// 只把一批副本追到同一个块末尾，就造成同 SSA 名的多条定义。那在真 SSA 里
/// 不会报错（读的都是名字），但 SSA→AST 重建按**基名塌缩 + 发射序**产出语句，
/// 副本会被**再执行一遍**：如果两份中间夹了对同一基名的写入，第二份算出的就是
/// 错值。已删的 SSA 层伪 `loop_tiling`（“把纯运算指令复制一份追加”）就是这条
/// 不变式的真实反例，当时实测 -O3 得 38 而应 32。
pub(super) fn duplicate_ssa_defs(func: &Function) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut dup: HashSet<String> = HashSet::new();
    for bb in func.blocks.values() {
        for phi in &bb.phis {
            if !seen.insert(phi.dest.name()) {
                dup.insert(phi.dest.name());
            }
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                if !seen.insert(d.name()) {
                    dup.insert(d.name());
                }
            }
        }
    }
    let mut out: Vec<String> = dup.into_iter().collect();
    out.sort();
    out
}

/// 由终结符重建 `bb.preds` / `bb.succs` —— 这两个字段是全函数唯一的缓存型 CFG 数据。
///
/// 为什么必须由本函数统一维护: `preds`/`succs` 是在接线时逐个 `push` 上去的, 而任何
/// 改写 terminator 的动作 (for 循环的旋转、`lower_switches_in_func`、SCCP 的常量 switch
/// 折叠、P19 跳转穿线) 都**不会**同步它们。而 `find_loop_body_multi` 这类分析又是沿
/// `preds` 反向走的 —— 读到的就是旧边。实测后果不是「少优化一点」而是重建出错:
/// `for (...) { if (i==1) continue; ... }` 的回边被发成空的 `if (i<n) {}`, 循环一次也不跑。
/// 本模块开头 `term_succs` / `collect_all_preds` 的注释早就写明「preds 会陈旧」,
/// 但没人负责让它不陈旧 —— 现在有两个读取点就有一份真源。
pub(super) fn refresh_cfg(func: &mut Function) {
    let preds = collect_all_preds(func);
    for bb in func.blocks.values_mut() {
        bb.succs = term_succs(&bb.term)
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        bb.preds = preds.get(&bb.name).cloned().unwrap_or_default();
    }
}

/// 从终结符派生后继的共享入点 (与 `term_succs` 同一真源, 供 `ssa` 内的 pass 使用)。
pub(super) fn term_succs_pub(term: &Terminator) -> Vec<String> {
    term_succs(term)
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}

/// Find the body blocks of a natural loop (header, latch).
/// Loop body = {header} ∪ {blocks that can reach latch without going through header}
///
/// 已 pub：「在环内」与「受头支配」不是同一个关系 —— 循环出口也受头支配
/// （实测 `while.end` 的支配集含 `while.cond`），拿支配关系当环内判据会把出口
/// 误当成环内后继，然后把整个 `while` 形态错拒。P16 的循环识别必须用这里的结果；
/// 另写一份自然循环收集只会与这里漂移。
///
/// 返回值按名字排序: 调用方会用它决定 phi 插入序, 直接 `HashSet::into_iter()`
/// 会把进程随机哈希序泄到下游。
pub(super) fn find_loop_body(func: &Function, header: &str, latch: &str) -> Vec<String> {
    find_loop_body_multi(func, header, std::slice::from_ref(&latch.to_string()))
}

/// 多 latch 版本的自然循环体收集。
///
/// 多个 latch (如 `if (c) continue;` 的 latch 与尾部回边共存) 必须全部参与
/// 反向 DFS: 只从单个 latch 出发会漏掉其它 latch 所在的分支区域, SSA→AST
/// 重建时那部分会被误判为“循环外后继”而发成 `break` (continue 语义丢失,
/// 循环提前终止)。
pub(super) fn find_loop_body_multi(
    func: &Function,
    header: &str,
    latches: &[String],
) -> Vec<String> {
    let mut body: HashSet<String> = HashSet::new();
    body.insert(header.to_string());
    // Reverse DFS from every latch, not going through header
    let mut stack: Vec<String> = latches
        .iter()
        .filter(|l| l.as_str() != header)
        .cloned()
        .collect();
    while let Some(bb) = stack.pop() {
        if !body.insert(bb.clone()) {
            continue;
        }
        if let Some(bb_data) = func.blocks.get(&bb) {
            for pred in &bb_data.preds {
                if pred != header && !body.contains(pred) {
                    stack.push(pred.clone());
                }
            }
        }
    }
    let mut v: Vec<String> = body.into_iter().collect();
    v.sort();
    v
}

/// 返回确定性的 BB 遍历序: 先按 `block_order` (创建序), 再补上 `blocks`
/// 里未出现在 `block_order` 的 key (按名字排序)。
///
/// 为什么需要: `blocks: HashMap<String, BasicBlock>` 的 `values()`/`keys()`
/// 迭代序由进程随机种子决定。任何“递增编号”或“首个胜出”型逻辑（如 alias set id、
/// 变量类型/声明序）一旦建立在它之上, 就会泄漏到产物字节里。同时保留
/// `block_order` 未能覆盖的块, 以免新增块静默丢失。
pub(super) fn ordered_block_names(func: &Function) -> Vec<String> {
    let mut emitted: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(func.blocks.len());
    for n in &func.block_order {
        if func.blocks.contains_key(n) && emitted.insert(n.clone()) {
            out.push(n.clone());
        }
    }
    let mut rest: Vec<&String> = func
        .blocks
        .keys()
        .filter(|k| !emitted.contains(k.as_str()))
        .collect();
    rest.sort();
    out.extend(rest.into_iter().cloned());
    out
}

/// Check if a block is the target of a back edge (cycle entry).
pub(super) fn is_cycle_entry(
    bb_name: &str,
    func: &Function,
    doms: &HashMap<String, HashSet<String>>,
) -> bool {
    for (name, bb) in &func.blocks {
        if !is_back_edge(name, bb_name, doms) {
            continue;
        }
        let succs = match &bb.term {
            Terminator::Jump(t) => vec![t.as_str()],
            Terminator::Branch { t, f, .. } => vec![t.as_str(), f.as_str()],
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                let mut s: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
                if !s.contains(&default.as_str()) {
                    s.push(default.as_str());
                }
                s
            },
            Terminator::Dispatch { targets } => targets.iter().map(|t| t.as_str()).collect(),
            _ => vec![],
        };
        if succs.contains(&bb_name) {
            return true;
        }
    }
    false
}

/// 计算每个块的前驱个数 (含回边; 同一终结符重复的目标只计一次)。
pub(super) fn predecessor_counts(func: &Function) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for bb in func.blocks.values() {
        let mut succs: Vec<&str> = Vec::new();
        match &bb.term {
            Terminator::Jump(t) => succs.push(t.as_str()),
            Terminator::Branch { t, f, .. } => {
                succs.push(t.as_str());
                succs.push(f.as_str());
            },
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                for tg in targets {
                    succs.push(tg.as_str());
                }
                succs.push(default.as_str());
            },
            Terminator::Dispatch { targets } => {
                for tg in targets {
                    succs.push(tg.as_str());
                }
            },
            Terminator::Ret { .. } | Terminator::Unreachable => {},
        }
        let mut uniq: HashSet<&str> = HashSet::new();
        for s in succs {
            if uniq.insert(s) {
                *counts.entry(s.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// 反向 CFG 的直接后支配者 (`ipdom[b] = j`: 从 `b` 走到函数出口的**全部**路径
/// 都必经过的、离 `b` 最近的那个块)。
///
/// 为什么需要它: 旧的 `branch_join` 只沿「唯一前驱 + 无条件跳转」链前进, 碰到
/// 二分支就放弃。于是「分支的某一条路径本身又是一个分支」时凝聚点被判为不存在,
/// `rebuild_bb` 就把凝聚块内联进**第一条**展开的路径并用共享的 `visited` 把它标成
/// 已处理 —— 另一条路径重建时它返回空, `If` 之后的所有语句只在条件为真时执行。
/// 实测: `x = a > 10 ? 1 : (a > 3 && a < 9);` 编译后整个主函数在赋值之后就提前
/// 返回, 后面的 `printf` 一句也不执行 (静默丢代码, 不是编不过)。
///
/// 算法: Cooper–Harvey–Kennedy 不动点迭代在反向 CFG (以虚拟出口为根) 上求支配树,
/// 即原 CFG 的后支配树。`Unreachable` / 不可达出口的块不进入结果。
pub(super) fn compute_ipdoms(func: &Function) -> HashMap<String, String> {
    /// 虚拟出口: 所有 `return` 块的共同后继。
    const EXIT: &str = "\u{1}[postdom-exit]";
    // 1. 正向后继 (`Ret` 额外指向虚拟出口)
    let mut succs: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, bb) in &func.blocks {
        let mut v: Vec<&str> = term_succs(&bb.term);
        if matches!(bb.term, Terminator::Ret { .. }) {
            v.push(EXIT);
        }
        succs.insert(name.as_str(), v);
    }
    // 2. 反向边
    let mut rev: HashMap<&str, Vec<&str>> = HashMap::new();
    rev.entry(EXIT).or_default();
    for (from, ss) in &succs {
        for s in ss {
            rev.entry(s).or_default().push(from);
        }
    }
    // 3. 反向 CFG 的逆后序 (RPO) 与编号
    let mut post: Vec<&str> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    visited.insert(EXIT);
    let mut stack: Vec<(&str, usize)> = vec![(EXIT, 0)];
    loop {
        let Some(&(n, idx)) = stack.last() else { break };
        let children: &[&str] = match rev.get(n) {
            Some(v) => v.as_slice(),
            None => &[],
        };
        if idx < children.len() {
            let c = children[idx];
            // 只能在子列表不再被借用时才拿可变引用
            let need_push = visited.insert(c);
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if need_push {
                stack.push((c, 0));
            }
        } else {
            post.push(n);
            stack.pop();
        }
    }
    post.reverse();
    let num: HashMap<&str, usize> = post
        .iter()
        .copied()
        .enumerate()
        .map(|(i, n)| (n, i))
        .collect();
    // 4. 不动点迭代 (Cooper–Harvey–Kennedy)
    //
    // 注意两张表用途不同:**遍历序**用反向图的邻接 (`rev`, 即从出口能走到谁),
    // 而求 `n` 的直接后支配者要看的是反向图的**前驱** = 正向 CFG 的**后继**
    // (`succs[n]`) —— 混用 `rev` 会把所有块的 idom 算成空。
    let mut idom: HashMap<&str, &str> = HashMap::new();
    idom.insert(EXIT, EXIT);
    let mut changed = true;
    while changed {
        changed = false;
        for n in post.iter().skip(1) {
            let forward_succs: &[&str] = match succs.get(n) {
                Some(v) => v.as_slice(),
                None => &[],
            };
            let mut cand: Option<&str> = None;
            for p in forward_succs {
                if !idom.contains_key(p) {
                    continue;
                }
                cand = Some(match cand {
                    None => *p,
                    Some(x) => intersect_post_dom(x, *p, &idom, &num),
                });
            }
            if let Some(d) = cand {
                if idom.get(n) != Some(&d) {
                    idom.insert(n, d);
                    changed = true;
                }
            }
        }
    }
    // 5. 去掉虚拟出口与自己, 只留真实块名的后支配关系
    let mut out: HashMap<String, String> = HashMap::new();
    for (n, d) in idom {
        if n == EXIT || d == EXIT || n == d {
            continue;
        }
        out.insert(n.to_string(), d.to_string());
    }
    out
}

/// 在 RPO 编号上取两个候选后支配者的最近公共祖先。
fn intersect_post_dom<'a>(
    mut a: &'a str,
    mut b: &'a str,
    idom: &HashMap<&'a str, &'a str>,
    num: &HashMap<&'a str, usize>,
) -> &'a str {
    while a != b {
        while num[a] < num[b] {
            b = idom[b];
        }
        while num[b] < num[a] {
            a = idom[a];
        }
    }
    a
}

/// Check if edge from→to is a back edge (to dominates from).
pub(super) fn is_back_edge(from: &str, to: &str, doms: &HashMap<String, HashSet<String>>) -> bool {
    doms.get(from).map(|d| d.contains(to)).unwrap_or(false)
}
