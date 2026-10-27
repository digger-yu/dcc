// src/ssa/defassign.rs - 「必定已赋值」数据流（P12 非结构化函数的准入判据）
//
// 放什么: `definitely_assigned` —— 每个基名是否在**每一条**通往使用的路径上都已被
//   定义过。只回答这个事实，不做任何分配决定（那在 regalloc.rs）。
// 不放什么: 活性/干涉（liveness.rs）、支配（cfg.rs）、家（regalloc.rs）。
//
// 为什么需要它（而不是继续用 `Span::entry_def`）：
//   `entry_def` 是「第一次出现必须是 entry 块里的定义」—— 那是结构化函数的代理判据，
//   对 goto 形态**过严到失真**：解释器主循环把 `base/pc/cl/k` 声明在函数开头、赋值
//   在 `newframe:` 标签之后（`cl = ci_func(ci); k = cl->p->k; pc = ...; base = ...`），
//   而那段代码支配全部使用点。按 entry_def 判，这四个最热的指针全部被拒，
//   「让它们驻留寄存器」就永远做不到。
//
//   `entry_def` 想挡的是另一件事：**某条路径上没初始化就读**（`int x; if (c) x = 1;
//   return x;` 里 x 在 else 路径读到的是调用者留在寄存器里的值）。这件事的正解就是
//   必定已赋值 —— 它同时比 entry_def 更准（放过 newframe 那种形态）和更严
//   （`int x; if (c) x = 1; else x = 2;` 在 entry_def 下是拒，在它下面是收）。
//
// 保守方向：宁少算「已赋值」。多算一个就是在未初始化读上把一个**未定义行为**固定成
// 「某个确定的寄存器值」，两档对拍会变成抛硬币。

use crate::ssa::cfg::ordered_block_names;
use crate::ssa::ir::{kind_operand_ssa_vars, Function};
use std::collections::{HashMap, HashSet};

/// 每个基名是否「处处都已赋值」。
///
/// 判据：**每一次**使用所在的位置上，该基名的至少一个版本必然已经执行过。
/// 实现是块级前向数据流 + 块内顺序：
///   `IN[b] = ∩ OUT[p]`（p 遍历 b 的全部前驱），`OUT[b] = IN[b] ∪ DEF[b]`。
/// 入口块的 `IN` 是形参（值由 ABI 送进来）。不可达块的前驱集为空 ⇒ `IN = ∅`
/// （不可达块里的使用一律不算「已赋值」；它们不会被发射，宁保守）。
pub fn definitely_assigned(func: &Function) -> HashSet<String> {
    let order = ordered_block_names(func);
    let index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let n = order.len();

    // 本块定义的全部基名（phi dest 也是定义）。
    let mut defs: Vec<HashSet<String>> = vec![HashSet::new(); n];
    for (i, name) in order.iter().enumerate() {
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        for phi in &bb.phis {
            defs[i].insert(phi.dest.base.clone());
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                defs[i].insert(d.base.clone());
            }
        }
    }

    // 前驱表从终结符派生（`bb.preds` 是构造期缓存，改过 terminator 即为脏）。
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, name) in order.iter().enumerate() {
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        for s in crate::ssa::cfg::term_succs_pub(&bb.term) {
            if let Some(&j) = index.get(s.as_str()) {
                preds[j].push(i);
            }
        }
    }

    let entry_idx = index.get(func.entry.as_str()).copied();
    let params: HashSet<String> = func.params.iter().map(|p| p.base.clone()).collect();

    // 可达性：不可达块对「必定已赋值」必须当作 ⊤（不提供任何约束），不能当作 ∅。
    //
    // 为什么这条是必需而不是保守：`build.rs` 把 `goto *expr` 之后的代码放进
    // `.dead` 块，而**紧跟其后的 label 会把那个死块的 terminator 从 Unreachable
    // 改成 Jump(label)**（label 降级只在 `Unreachable` 时才连线）。于是 label BB
    // 多出一个永远不执行的前驱，一条「求交」的数据流会把它当成「什么都没赋值」，
    // 把整张 label 表后面的 `base/pc/cl/k` 全判成未赋值 —— 实测就是这个前提让
    // 四个指针在 analysis-only 通道上依旧全被拒。
    let reachable: Vec<bool> = {
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        if let Some(e) = entry_idx {
            seen[e] = true;
            stack.push(e);
        }
        while let Some(i) = stack.pop() {
            let Some(name) = order.get(i) else { continue };
            let Some(bb) = func.blocks.get(name) else {
                continue;
            };
            for s in crate::ssa::cfg::term_succs_pub(&bb.term) {
                if let Some(&j) = index.get(s.as_str()) {
                    if !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        seen
    };

    // 不动点：**从 ⊤ 往下收**。
    //
    // 这一步不能从空集或「前驱已定义集」往上长：求交是单调减的方向，从小的初值
    // 出发会把自己锁在一个过小的不动点上（循环里的块永远拿不回走过一圈才有的
    // 赋值）。实测就是这个错法：`luaV_execute` 的 799 个块里 `label.startfunc`
    // 的 IN 被算成不含形参 `L`，连带 `base/pc/cl/k` 全被拒。正确做法是标准
    // must 分析的初值：全部可达块 = 全集，入口 = 形参，再迭代求交。
    let mut universe: HashSet<String> = params.clone();
    let note = |s: &mut HashSet<String>, v: &crate::ssa::SsaVar| {
        s.insert(v.base.clone());
    };
    for bb in func.blocks.values() {
        for phi in &bb.phis {
            note(&mut universe, &phi.dest);
            for (v, _) in &phi.incoming {
                note(&mut universe, v);
            }
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                note(&mut universe, d);
            }
            for v in kind_operand_ssa_vars(&inst.kind) {
                note(&mut universe, &v);
            }
        }
        for v in crate::ssa::ir::term_operand_ssa_vars(&bb.term) {
            note(&mut universe, &v);
        }
    }
    let mut in_sets: Vec<HashSet<String>> = vec![universe.clone(); n];
    let mut out_sets: Vec<HashSet<String>> = vec![universe; n];
    if let Some(e) = entry_idx {
        in_sets[e] = params.clone();
        out_sets[e] = params.union(&defs[e]).cloned().collect();
    }
    loop {
        let mut changed = false;
        for i in 0..n {
            if !reachable[i] {
                continue;
            }
            let new_in: HashSet<String> = if Some(i) == entry_idx {
                params.clone()
            } else {
                let mut acc: Option<HashSet<String>> = None;
                for &p in &preds[i] {
                    // 不可达前驱按 ⊤ 处理（跳过）：它不约束任何事实。
                    if !reachable[p] {
                        continue;
                    }
                    acc = Some(match acc {
                        None => out_sets[p].clone(),
                        Some(a) => a.intersection(&out_sets[p]).cloned().collect(),
                    });
                }
                // 可达非入口块必有可达前驱（否则它不可达）。防御性回退到 ⊤。
                acc.unwrap_or_else(|| in_sets[i].clone())
            };
            let new_out: HashSet<String> = new_in.union(&defs[i]).cloned().collect();
            if in_sets[i] != new_in || out_sets[i] != new_out {
                in_sets[i] = new_in;
                out_sets[i] = new_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // 逐点走一遍：任何一次使用不在「此刻已赋值」集合里，就把该基名标成不安全。
    let mut bad: HashSet<String> = HashSet::new();
    // `DCC_DEFASSIGN_DUMP=1`：把「第一次被判定为未赋值」的现场记下来（块 + 位置）。
    // 没有它，一个「没绑上」就只能看到 `not-defined-in-entry-block` 这行字。
    let dump = std::env::var("DCC_DEFASSIGN_DUMP").is_ok();
    let mut bad_at: HashMap<String, String> = HashMap::new();
    let first_use = |bad: &mut HashSet<String>,
                     bad_at: &mut HashMap<String, String>,
                     avail: &HashSet<String>,
                     v: &crate::ssa::SsaVar,
                     at: &str| {
        if !avail.contains(&v.base) {
            bad.insert(v.base.clone());
            if dump {
                bad_at
                    .entry(v.base.clone())
                    .or_insert_with(|| at.to_string());
            }
        }
    };
    for (i, name) in order.iter().enumerate() {
        // 不可达块里的使用一律不算「已赋值」（它们不会被发射）。
        if !reachable[i] {
            continue;
        }
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        let mut avail: HashSet<String> = in_sets[i].clone();
        // phi 的入值在前驱末尾求值；这里按「块入口即使用」处理（保守方向）。
        for phi in &bb.phis {
            for (v, _) in &phi.incoming {
                first_use(&mut bad, &mut bad_at, &avail, v, &format!("{name}:phi"));
            }
        }
        for (k, inst) in bb.insts.iter().enumerate() {
            for v in kind_operand_ssa_vars(&inst.kind) {
                first_use(&mut bad, &mut bad_at, &avail, &v, &format!("{name}:inst{k}"));
            }
            if let Some(d) = &inst.dest {
                avail.insert(d.base.clone());
            }
        }
        for v in crate::ssa::ir::term_operand_ssa_vars(&bb.term) {
            first_use(&mut bad, &mut bad_at, &avail, &v, &format!("{name}:term"));
        }
    }
    if dump {
        let mut names: Vec<&String> = bad.iter().collect();
        names.sort();
        eprintln!(
            "dcc: defassign {} blocks={n} reachable={}",
            func.name,
            reachable.iter().filter(|r| **r).count()
        );
        for n in names {
            let at = bad_at.get(n).map(|s| s.as_str()).unwrap_or("?");
            eprintln!("  defassign bad {n} at {at}");
        }
    }

    // 结果 = 「在 IR 里出现过」减去 bad。没出现过的名字不在这里回答（交给调用方的
    // `no-ir-presence` 拒因），避免把「没有生命期」误说成「已赋值」。
    let mut seen: HashSet<String> = HashSet::new();
    for bb in func.blocks.values() {
        for phi in &bb.phis {
            seen.insert(phi.dest.base.clone());
            for (v, _) in &phi.incoming {
                seen.insert(v.base.clone());
            }
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                seen.insert(d.base.clone());
            }
            for v in kind_operand_ssa_vars(&inst.kind) {
                seen.insert(v.base.clone());
            }
        }
        for v in crate::ssa::ir::term_operand_ssa_vars(&bb.term) {
            seen.insert(v.base.clone());
        }
    }
    seen.retain(|b| !bad.contains(b));
    seen
}
