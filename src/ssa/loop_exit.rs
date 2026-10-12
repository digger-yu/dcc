// src/ssa/loop_exit.rs - 多出口循环的出口归属 (判据与发射器的**同一真源**)
//
// 放什么: LoopExitOwner / LoopExitPlan / ExitClass / loop_shape / loop_exit_plan /
//   loop_exit_owner / owner_of / exit_class_of / resolve_loop_exit_stub / is_eh_pad /
//   has_multi_exit_cycle。
// 不放什么: 出口边的**实际发射** (在 rebuild.rs)。
// 关键纪律: 出口归属的判据与发射器必须同源 —— has_multi_exit_cycle / loop_exit_plan
//   与 rebuild.rs 里各出口边发射点调同一个 loop_exit_plan, 改一边必须同时看另一边。

use std::collections::{HashMap, HashSet};

use crate::ssa::cfg::{compute_dominators, find_loop_body_multi, is_back_edge, term_succs};
use crate::ssa::inline_temps::is_synthetic_temp_base;
use crate::ssa::ir::{Function, Terminator};
use crate::ssa::rebuild::{is_loop_exit_pure_return, is_pass_hint_inst};

/// 循环出口边的**所有权分类** —— 判据 (`has_multi_exit_cycle`) 与重建发射器
/// (`rebuild_bb` 的 3 个点 + `rebuild_cycle` 头块分支的 2 个点) 的**唯一真源**。
///
/// 为什么必须是单一真源: 历史上这一族两次被 gcc 差分当场证伪 (§7.28 / §7.32), 两次都
/// 是「判据说可以 / 发射器某一侧另外安排」的错位 —— 只改一侧会拆掉现有的互相掩盖
/// (见 §7.32 与本文件 `has_multi_exit_cycle` 的说明)。设计全文见
/// `docs/wp_multi_exit_loop.md`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LoopExitOwner {
    /// 循环的**自然落点 (续体)**: 由 `rebuild_cycle` 在 `while` 之后发射**一次**;
    /// 到达它的边只发 `break` (落到同一位置), 绝不得就地展开 (否则双份执行)。
    Continuation,
    /// 体内 `return` 的落点: 在**到达边**就地发 `Stmt::Return`。它不占用续体名额。
    PureReturn,
    /// 只被循环内路径到达、且自带语句的侧出口: 在**到达边**就地展开本块语句,
    /// 再由其 terminator 递归成 `break`。
    InlineOnEdge,
    /// 发射器无路可发 ⇒ 判据必须把整个函数退出 SSA 往返 (不优化且正确)。
    Unsupported,
}

/// 一个自然循环的出口所有权计划。
#[derive(Clone, Debug)]
pub(super) struct LoopExitPlan {
    /// 唯一确定的自然落点 (桩块折叠后的块名)。`None` = 本循环没有结构落点
    /// (所有出口都是体内 `return`, 循环体是 `while(1)` 永不落出)。
    pub cont: Option<String>,
    /// 每条出口边是否都能被发射器安排。`false` ⇒ 判据必须挡住本函数。
    pub expressible: bool,
    /// 节点头的 `Branch` 是否允许被**提升**成 `while (cond)`。
    ///
    /// 这是本轮查出的根因级约束: 提升头块分支 = 把头块那条出口边**改判**成循环的
    /// 落出口 (因为提升后头块不再发 `if`, 体内那条边根本没有发射点)。于是
    /// 「续体取法」与「提升许可」互为前提: 只有当头块的出口边**就是**续体时才能提升,
    /// 否则必须退回 `while(1) { if (c) return …; … }` 的形态。§7.32 那次放松把续体改对
    /// 却没管提升, 实测就是 `该走到 return 7: got 1 want 7` 那一类错值。
    pub allow_header_lift: bool,
    /// 拒判原因 (仅诊断窗口读, 不参与任何语义)。
    pub reason: &'static str,
}

/// 循环的**唯一结构事实**: latch 集合与 body 集合。
///
/// 判据与重建共用这一份 —— 两边各算一次就会因 `preds`/`succs` 缓存过期而给出不同的
/// 循环体 (§7.32 未修项 2 的同类问题), 而「判据放行 ⇔ 发射器有路可发」的同源性
/// 正好建立在两边看到的是同一个循环之上。返回的 body **含**头块; 无回边时返回 `None`。
pub(super) fn loop_shape(
    func: &Function,
    header: &str,
    doms: &HashMap<String, HashSet<String>>,
) -> Option<(Vec<String>, HashSet<String>)> {
    let mut latches: Vec<String> = Vec::new();
    let mut names: Vec<&String> = func.blocks.keys().collect();
    names.sort();
    for name in names {
        if name == header {
            continue;
        }
        // C++ 零成本异常：落地垫块**不是循环体的一部分**。垫块会 `goto` 回入正常规块
        // （包括循环头），那条边在 CFG 上就是一条「回边」，于是垫块被算成 latch、垫块
        // 自己与它的链被圈进 body，垫块的去向（如 `eh_resume.8`）进而被数成循环的
        // 「共享出口」当上续体 —— 循环正常走完后就落进 `_Unwind_Resume`（实测
        // `eh_loop_and_scope_exit.cpp`：`PLAN cont=Some("eh_resume.8")`，g++ 退出码 0 /
        // dcc -3）。垫块只由 unwinder 按地址区间与其他块的 `goto` 进入，它既不能当
        // latch、也不能当体块或续体；它的发射归 `rebuild_eh_blocks` 平铺那一段。
        if is_eh_pad(name, func) {
            continue;
        }
        if !is_back_edge(name, header, doms) {
            continue;
        }
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        // 后继从 terminator 现取: `term_succs` 的文档写明 SSA pass 改过 terminator 后
        // `b.succs` 可能过期, 读旧边集会**漏掉真存在的出口边** (静默误编译的方向)。
        if term_succs(&bb.term).iter().any(|s| *s == header) {
            latches.push(name.clone());
        }
    }
    if latches.is_empty() {
        return None;
    }
    let mut body: HashSet<String> = find_loop_body_multi(func, header, &latches)
        .into_iter()
        .collect();
    // 同上：body 里也要去掉垫块，否则从体块跳向垫块的那条边会被当普通体内边。
    body.retain(|b| !is_eh_pad(b, func));
    Some((latches, body))
}

/// 该块是否属于**平坦发射的 EH 落地垫区**（只由 unwinder 按 `.text` 地址区间、以及其他
/// 块的 `goto` 进入）。它不是正常控制流的一部分：既不能当循环体 / latch，也不能当
/// 循环续体；它的发射归 `rebuild_eh_blocks`，而结构化重建碰到它只能发一个 `goto`。
pub(super) fn is_eh_pad(name: &str, func: &Function) -> bool {
    func.eh_unwind_blocks.iter().any(|p| p == name)
}

/// 把出口块的**空壳落点**折叠到它真正干活的后继块。
///
/// 源码里一句 `break;` 落到的块常常**自己没有语句** (全部语义在它的后继里, 实测
/// `if.then.0` = 零条语句 + 无 phi + `Jump(for.end)`); 不折叠就会把桩块与它身后的
/// 合并块数成两个出口 —— 于是今天连「`for` 里一个普通 `break`」都被退路挡住 (实测
/// 见 §7.32 表格与本文件 `has_multi_exit_cycle` 的说明)。
///
/// 折叠条件严格: 只沿「无自己的语句 + 无 phi + terminator 是无条件 `Jump` + 目标仍在
/// 循环外」的链走; 一旦折回循环内、或碰到有语句/有 phi/非 `Jump` 的块就停 (停下时的
/// 块就是代表块, 即使用它自己仍是桩块 —— 那是一条回入边, 所有权归续体以外的保守分类)。
pub(super) fn resolve_loop_exit_stub(
    target: &str,
    func: &Function,
    header: &str,
    body: &HashSet<String>,
) -> String {
    let mut cur = target.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.clone()) {
            return cur;
        }
        let Some(bb) = func.blocks.get(&cur) else {
            return cur;
        };
        if !bb.phis.is_empty() || !bb.insts.iter().all(|i| is_pass_hint_inst(&i.kind)) {
            return cur;
        }
        let Terminator::Jump(n) = &bb.term else {
            return cur;
        };
        if n.as_str() == header || body.contains(n.as_str()) {
            return cur;
        }
        cur = n.clone();
    }
}

/// 出口块在**还不知道续体**时的粗分类 (续体确定后再定最终所有权)。
///
/// `Shared` = 既不是体内纯 `return`、也不能在到达边展开 ⇒ 必须由循环后的续体发一次。
/// 这一档就是「自然落点的候选」, 也就是今天 `has_multi_exit_cycle` 真正该数的东西。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExitClass {
    /// `Ret` 终结符、无 phi、单一前驱: 到达边直接发 `Stmt::Return` 就够。
    /// 前驱集合的约束保留 (§7.32): 多前驱的 `Ret` 块汇聚的是**不同路径的值**, 按路径
    /// 展开要依赖 env 里的版本恰好一致, 那是另一件事, 不在本轮放行范围。
    Ret,
    /// 自带语句且**全部前驱都在循环内**: 只能被循环路径到达 ⇒ 到达边展开不会双份执行。
    ///
    /// (旧发射器额外还禁止「头块直接出口」与「latch 的后继」展开 —— 那两条约束说的
    /// 就是「本块是否那个只发一次的续体」, 现在由计划的 `cont` 唯一确定并**优先**判为
    /// `LoopExitOwner::Continuation`, 不必再单独表达。旧发射器只给 5 个发射点中的 3 个
    /// 用 (§7.32 未修项 1), 如今 5 个点走同一个 [`loop_exit_owner`]。)
    Inline,
    /// 其余: 循环外也有前驱进入它 ⇒ 它必须是那个「发一次」的落点。
    Shared,
    /// 这条边的落点是 EH 落地垫块。垫块不参与结构化续体（它由 `rebuild_eh_blocks` 平铺
    /// 发射，带自己的标签），所以它**既不消耗也不提供**续体名额；发射器在到达边递归到
    /// 它时，由 `rebuild_bb` 开头的发垫守卫写成 `goto pad`。
    ///
    /// 为什么单独建一个类而不是归到 `Inline`：`Inline` 的含义是「本块语句属于循环路径的
    /// 一部分，在到达边展开」，把垫块的语句搬到边上是错的（垫块入口地址要进
    /// `.eh_frame`/落地垫表，语句必须只有一份）。
    IntoPad,
}

fn exit_class_of(folded: &str, func: &Function, header: &str, body: &HashSet<String>) -> ExitClass {
    let Some(bb) = func.blocks.get(folded) else {
        return ExitClass::Shared;
    };
    // 落点就是 EH 落地垫：它不属于结构化续体（见 `ExitClass::IntoPad`）。这一条必须在
    // 所有其他分类之前，否则垫块会被归成 `Shared` 而成为续体候选（实测就是这么错的）。
    if is_eh_pad(folded, func) {
        return ExitClass::IntoPad;
    }
    // 这里**不**对落地垫块做特判。落地垫不得被结构化重建消费这件事只有一个拦截点：
    // `rebuild_bb` 开头（它同时管住了标签发射与 visited 记账，是「已发射」的唯一真源）。
    // 在此处再把垫块归成 `Shared` 会改变续体选取，而续体选取正是发射器依赖的事实；
    // 本轮实测：加上这道守卫后 `eh_loop_and_scope_exit` 的落地垫分派错位（g++ 0 /
    // dcc -3），去掉它只保留 `rebuild_bb` 那一处则标签与行为同时正确 —— 同一件事写两处
    // 必然互相拧。
    // 体内纯 `return`: 与 `is_loop_exit_pure_return` 同一判据 (Ret + 无 phi + 单一前驱)。
    if is_loop_exit_pure_return(folded, func) {
        return ExitClass::Ret;
    }
    if bb.phis.is_empty()
        && !bb.insts.iter().all(|i| is_pass_hint_inst(&i.kind))
        && bb
            .preds
            .iter()
            .all(|p| p.as_str() == header || body.contains(p.as_str()))
    {
        return ExitClass::Inline;
    }
    ExitClass::Shared
}

/// 计算一个自然循环的出口所有权计划。
pub(super) fn loop_exit_plan(
    func: &Function,
    header: &str,
    body: &HashSet<String>,
    latches: &[String],
    doms: &HashMap<String, HashSet<String>>,
) -> LoopExitPlan {
    let in_loop = |x: &str| x == header || body.contains(x);
    let fold = |x: &str| resolve_loop_exit_stub(x, func, header, body);

    // 1. 出口边 (src -> raw target), 按 src 字典序保证确定性。
    let mut srcs: Vec<String> = body.iter().cloned().collect();
    if !srcs.iter().any(|s| s == header) {
        srcs.push(header.to_string());
    }
    srcs.sort();
    let mut edges: Vec<(String, String)> = Vec::new();
    for name in srcs.iter() {
        let Some(b) = func.blocks.get(name) else {
            continue;
        };
        for s in term_succs(&b.term) {
            if s == header || body.contains(s) || is_back_edge(name, s, doms) {
                continue;
            }
            if !edges.iter().any(|(a, t)| a == name && t == s) {
                edges.push((name.clone(), s.to_string()));
            }
        }
    }

    // 2. 粗分类 (折叠后), 得到「必须由循环后续体发一次」的共享出口集合。
    let mut shared: Vec<String> = Vec::new();
    let mut edge_class: Vec<(String, String, ExitClass)> = Vec::new();
    for (src, tgt) in edges.iter() {
        let f = fold(tgt);
        let c = exit_class_of(&f, func, header, body);
        if c == ExitClass::Shared && !shared.iter().any(|e| *e == f) {
            shared.push(f.clone());
        }
        edge_class.push((src.clone(), tgt.clone(), c));
    }

    // 3. 两个**结构**候选: 节点头条件边的落出口 (提升后它就是落出口) 与 latch 的尾条件落出。
    let header_exit: Option<String> = match func.blocks.get(header).map(|b| b.term.clone()) {
        Some(Terminator::Branch { ref t, ref f, .. }) => {
            let tc = in_loop(t) || is_back_edge(header, t, doms);
            let fc = in_loop(f) || is_back_edge(header, f, doms);
            if tc && !fc {
                Some(f.clone())
            } else if fc && !tc {
                Some(t.clone())
            } else {
                None
            }
        },
        _ => None,
    };
    // 头块那条体外出边如果落在 EH 落地垫上，它就不是结构续体（垫块只由 unwinder/`goto`
    // 进入）。不滤掉的话 `allow_header_lift` 与 `cont` 都会把「提升后的落出口」算成垫块，
    // 循环正常结束就走进 `_Unwind_Resume`。
    let header_exit = header_exit.filter(|h| !is_eh_pad(&fold(h), func));
    // 节点头是**无条件 `Jump` 且直接跳出循环** = 循环体无法从头块到达的退化形状。
    // 发射器对它只有两种发法 (`rebuild_cycle` 的 `body_entries` 只收**体内**后继):
    // 发 `while (1) { <头块语句> }` 后直接落到续体 (真实循环体一条都不执行),
    // 或因头块自身不可达而死循环 —— 两者都是静默错值, 没有第三种发法可安排那条边。
    // 今天旧判据在这里并不挡 (体外后继只有 1 个), 属于「判据比发射器宽」的反方向
    // (过宽), 因此这一条同时收紧判据。
    let header_jump_escape: Option<String> = match func.blocks.get(header).map(|b| &b.term) {
        Some(Terminator::Jump(t)) if !in_loop(t) && !is_back_edge(header, t, doms) => {
            Some(t.clone())
        },
        _ => None,
    };
    let mut tail_exits: Vec<String> = Vec::new();
    for l in latches.iter() {
        let Some(b) = func.blocks.get(l) else {
            continue;
        };
        for s in term_succs(&b.term) {
            if s == header || body.contains(s) || is_back_edge(l, s, doms) {
                continue;
            }
            let fs = fold(s);
            // 落在 EH 落地垫上的边不是续体候选（垫块由平铺那一段发），否则循环后会
            // 把 `_Unwind_Resume` 当正常落点发出去。
            if is_eh_pad(&fs, func) {
                continue;
            }
            if !tail_exits.iter().any(|e| *e == fs) {
                tail_exits.push(fs);
            }
        }
    }

    // 4. 续体唯一化。**共享出口优先**: 它们是「循环外也有前驱进入」的块, 除当作落点
    //    外没有别的发法; 只有当一条共享出口都没有时, 才在两个结构候选里取唯一值。
    let cont: Option<String>;
    let mut reason = "";
    if shared.len() == 1 {
        cont = Some(shared[0].clone());
    } else if shared.len() > 1 {
        cont = None;
        reason = "两个以上必须共享的出口块";
    } else if tail_exits.len() == 1 {
        cont = Some(tail_exits[0].clone());
    } else if tail_exits.len() > 1 {
        cont = None;
        reason = "两个latch各自落出到不同的块";
    } else if let Some(h) = header_exit.as_ref() {
        cont = Some(fold(h));
    } else {
        // 无限循环且所有出口都是体内 return: 根本没有落出口, 循环之后不得发任何块。
        cont = None;
        reason = "无落出口(全 return)";
    }

    // 5. 逐边定所有权; 任何一条边走不到发射器的三种发法之一就是无路可发。
    //
    //    本轮实测新增的硬约束 (`docs/wp_multi_exit_loop.md` §2.4 的 A9):
    //    **节点头自己就是循环体块**（旋转后的 do-while / 无限循环），它带对**具名存储**的
    //    写，而它那条出口边折到**由 latch 提供的**续体（即头块上真有一句 `break`）——
    //    两种发法实测都错：
    //      * 提升 `Branch` 为 `while (cond)` ⇒ 头块指令被整体丢弃，具名变量的写被追到
    //        使用点重算，相对顺序变了（`do{s+=i;i++;if(i==3)break;}while(i<n);return s+i;`
    //        实测 -O2 `do_break(3)` 得 9 而 gcc 得 6）；
    //      * 退回 `while(1){ if(c){…} else {…} }` 形态 ⇒ 本轮实测同样错（同一读数）。
    //    缺的机制是把头块的具名写固定成“循环体内按原序发射的语句”并与提升互斥（需重做
    //    `header_insts_droppable` 与头块指令的发射路径），不在本轮放行集内 ⇒ 整函数退回
    //    AST（不优化但正确，实测：`DCC_OPT_SKIP=run_ssa_passes` 下该形状逐档与 gcc 一致）。
    //
    //    `!tail_exits.is_empty()` 是本条约束的**必要限定**，不是修饰: 只有当续体是**别的
    //    latch 块**落出来的、头块那条边是额外的 `break` 时，才存在「头块既是体块又是分支点」
    //    的冲突。`while (k-- > 0) { … }` 形态里头块就是唯一落出的条件块（没有独立 latch
    //    落出），那条边是循环条件本身而不是体内 `break` ⇒ 不该被挡。少了这个限定会把
    //    `while(n--)` 这一整族 C 里最常见的循环踢出 SSA（相对 HEAD 的**覆盖面倒退**，实测
    //    `DCC_SSA_BAIL=1` 会报 `共享续体的循环 (f_while_kdec)`），而倒退不会让任何测试变红。
    let header_writes_user_storage = func.blocks.get(header).map_or(false, |b| {
        b.insts.iter().any(|i| {
            !is_pass_hint_inst(&i.kind)
                && i.dest
                    .as_ref()
                    .is_some_and(|d| !is_synthetic_temp_base(func, &d.base))
        })
    });
    let header_break_with_named_write = header_exit.as_ref().is_some_and(|h| {
        !tail_exits.is_empty()
            && header_writes_user_storage
            && cont.as_deref() == Some(fold(h).as_str())
    });
    let mut expressible = shared.len() <= 1
        && tail_exits.len() <= 1
        && header_jump_escape.is_none()
        && !header_break_with_named_write;
    if header_jump_escape.is_some() && reason.is_empty() {
        reason = "节点头直跳循环外(循环体不可达)";
    }
    if header_break_with_named_write && reason.is_empty() {
        reason = "节点头带具名写 + 头块 break 到续体(两种发法实测都错)";
    }
    for (_, tgt, c) in edge_class.iter() {
        let owner = owner_of(tgt, *c, cont.as_deref(), func, header, body, doms);
        if owner == LoopExitOwner::Unsupported {
            expressible = false;
            if reason.is_empty() {
                reason = "出口边无路可发";
            }
        }
    }

    // 6. 提升许可: 只有当节点头那条出口边**就是**续体时才允许把 `Branch` 提成
    //    `while (cond)`; 否则体内那条边的语句/返回值随提升整个丢掉 (§7.32 那 14 条错值的
    //    根因)。
    //
    //    第二个条件同样是实测出来的 (本轮 adv 夹具 A9
    //    `do { s+=i; i++; if (i==3) break; } while (i<n); return s+i;`): 提升 = **整体丢弃**
    //    头块的指令, 而头块里对**具名存储**的写一旦被丢, 重建只能在读到它的地方重算,
    //    写相对其它语句的时机就被挪动 (实测 -O2 `do_break(3)` 得 9 而 gcc 得 6 —— `s+=i`
    //    用了自增之后的 i)。发射器原有的 `header_insts_droppable` 本想拦这类形状, 但它只
    //    问「dest 是否被登记为可内联」, 而具名变量的版本 (`i.2 = _tupd_2 + 0`) 恰恰是
    //    可内联的拷贝 ⇒ 它对「写具名变量」判成可丢弃。
    //
    //    写在计划里而不是只写在发射器里, 是为了保持本工作包的第一原则: 判据、续体取法、
    //    提升许可、出口边守卫**同源** (§7.32 的教训是单侧改动会拆掉现有的互相掩盖)。
    //    (上面第 5 步已把“头块具名写 + 头块 break”整族挡成 `expressible = false`, 这里的
    //    否定提升仍然保留: 它拦的是头块出口边**不是**续体、但仍带具名写的那些形状。)
    let allow_header_lift = match header_exit.as_ref() {
        None => true,
        Some(h) => cont.as_deref() == Some(fold(h).as_str()) && !header_writes_user_storage,
    };

    if std::env::var("DCC_LOOPDBG").is_ok() {
        eprintln!("[loopdbg] PLAN header={header} cont={cont:?} pads={:?}", func.eh_unwind_blocks);
        eprintln!(
            "[loopdbg]   shared={shared:?} tail_exits={tail_exits:?} header_exit={header_exit:?} expressible={expressible} reason={reason}"
        );
        for (s, t, c) in edge_class.iter() {
            eprintln!("[loopdbg]   edge {s} -> {t} folded={} class={:?}", fold(t), c);
        }
    }

    LoopExitPlan {
        cont,
        expressible,
        allow_header_lift,
        reason,
    }
}

/// 单条出口边的所有权 —— 计划内部与 `rebuild_bb` 守卫共用的分类函数。
fn owner_of(
    target: &str,
    cls: ExitClass,
    cont: Option<&str>,
    func: &Function,
    header: &str,
    body: &HashSet<String>,
    _doms: &HashMap<String, HashSet<String>>,
) -> LoopExitOwner {
    let folded = resolve_loop_exit_stub(target, func, header, body);
    if cont == Some(folded.as_str()) {
        return LoopExitOwner::Continuation;
    }
    match cls {
        ExitClass::Ret => LoopExitOwner::PureReturn,
        ExitClass::Inline => LoopExitOwner::InlineOnEdge,
        // 落点在垫块区：发射器在到达边递归到它时由 `rebuild_bb` 的发垫守卫写成 `goto pad`，
        // 而垫块正文与标签由平铺那一段唯一发出。它不占续体名额，也不参与展开。
        ExitClass::IntoPad => LoopExitOwner::InlineOnEdge,
        // 桩块自身无语句: 折叠后的块如果是上述两类, 同样能在到达边安排
        ExitClass::Shared => {
            if folded != target {
                match exit_class_of(&folded, func, header, body) {
                    ExitClass::Ret => return LoopExitOwner::PureReturn,
                    ExitClass::Inline => return LoopExitOwner::InlineOnEdge,
                    ExitClass::IntoPad => return LoopExitOwner::InlineOnEdge,
                    ExitClass::Shared => {},
                }
            }
            LoopExitOwner::Unsupported
        },
    }
}

/// 重建侧查询单条出口边的所有权 (判据与发射器的同一真源)。
pub(super) fn loop_exit_owner(
    target: &str,
    func: &Function,
    header: &str,
    body: &HashSet<String>,
    doms: &HashMap<String, HashSet<String>>,
) -> LoopExitOwner {
    let (latches, _) = match loop_shape(func, header, doms) {
        Some(v) => v,
        None => return LoopExitOwner::Unsupported,
    };
    let plan = loop_exit_plan(func, header, body, &latches, doms);
    let cls =
        exit_class_of(&resolve_loop_exit_stub(target, func, header, body), func, header, body);
    owner_of(target, cls, plan.cont.as_deref(), func, header, body, doms)
}

/// 结构化重建**表达不了**的循环形状：本循环的出口边里有发射器安排不了的归属。
///
/// 与旧版的区分（旧版数「循环外后继个数 > 1」）：
///   * 旧判据把**桩块** (`break` 的空壳落点, 零条语句、无 phi、`Jump` 到合并块) 与它
///     身后的合并块数成两个出口 ⇒ 今天连「`for` 里一个普通 `break`」都整函数退 SSA
///     (实测形状清单见 `docs/wp_multi_exit_loop.md` §2, 其中 S5/S7/S13 本无需挡);
///   * 同时旧判据又**没数**真正的硬约束：同一个循环有两个结构落点候选 (节点头条件边与
///     latch 尾条件边落到不同的块) 时 `while` 只能表达一个。
///
/// 现在判据与发射器**同源**：`has_multi_exit_cycle` 与 `rebuild_bb`/`rebuild_cycle` 的 5 + 1 个
/// 出口边发射点 (`SwitchTable` 的出边是本轮实测查出的第 6 个) 都调同一个 [`loop_exit_plan`]
/// ⇒ 「判据放行」等价于「每条出口边都有发法」。
/// 任一侧单独改都会拆掉现有的互相掩盖而产出错值 (§7.28 三条改法、§7.32 的放松均如此),
/// 所以这一轮是「发射点守卫 + 续体取法 + 提升许可 + 判据」同一笔改完。
///
/// 与聚合整体赋值同一政策：IR/重建表达不了的形状**整函数退出 SSA 往返**（不优化且正确）。
pub fn has_multi_exit_cycle(func: &Function) -> bool {
    let doms = compute_dominators(func);
    let mut headers: Vec<&String> = func.blocks.keys().collect();
    headers.sort();
    for header in headers {
        let Some((latches, body)) = loop_shape(func, header, &doms) else {
            continue;
        };
        if !loop_exit_plan(func, header, &body, &latches, &doms).expressible {
            return true;
        }
    }
    false
}
