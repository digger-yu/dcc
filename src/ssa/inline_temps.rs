// src/ssa/inline_temps.rs - 合成临时值的内联窗口 (SSA→AST 重建的前置决策)
//
// 放什么: ExpandedTemp / InlinePlan / plan_inlined_temps / inline_window_safe /
//   is_synthetic_temp_base / is_addr_producer / find_use_site。
// 不放什么: 出口边的归属判据在 loop_exit.rs; 单条指令→表达式的翻译在 expr_rebuild.rs;
//   控制流重建与内联结果的实际发射在 rebuild.rs。
// 注: plan_inlined_temps 必须在生成局部声明之前调用 —— 未内联的临时值会退化成一条
//   赋值语句, 其名字必须进局部声明表, 声明序因此依赖内联决策。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Expr};

use crate::ssa::expr_rebuild::{
    collect_op_uses, collect_op_uses_op, inst_to_expr_resolved, op_count_in_kind, op_count_in_op,
};
use crate::ssa::ir::{
    inst_clobbers_memory, inst_reads_memory, inst_unremovable, inst_written_bases,
    kind_operand_ssa_vars, Function, Inst, InstKind, Operand, OrderedNameSet, Terminator,
};
use crate::ssa::rebuild::{inst_has_side_effect, is_lower_ir_temp_base, is_pass_hint_inst};

/// `name` 在指令/terminator 中的**首个**使用点: (块名, 序号), 序号 == insts.len()
/// 表示它出现在该块的 terminator 上。
fn find_use_site(func: &Function, name: &str) -> Option<(String, usize)> {
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for (i, inst) in bb.insts.iter().enumerate() {
            if op_count_in_kind(&inst.kind, name) > 0 {
                return Some((bb_name.clone(), i));
            }
        }
        let in_term = match &bb.term {
            Terminator::Ret { val: Some(o) } => op_count_in_op(o, name) > 0,
            Terminator::Branch { cond, .. } => op_count_in_op(cond, name) > 0,
            Terminator::SwitchTable { idx, .. } => op_count_in_op(idx, name) > 0,
            _ => false,
        };
        if in_term {
            return Some((bb_name.clone(), bb.insts.len()));
        }
    }
    None
}

/// `name` 是否被某个块的 **phi** 读（作为 incoming）。
///
/// phi 的入边**不在** `find_use_site`/`use_count` 的视野里（它们只扫 inst 与
/// terminator），但 `rebuild_cycle` 会把头块 phi 的 dest **别名**到第一个 incoming
/// （基名塌缩），于是「读那个 phi」在 AST 里就是「读那个名字的槽」。所以一个只被
/// phi 读的合成临时值不是「无人读」，而是**那个槽的初始化**：把它的定义当内联丢掉，
/// 槽就再也没人写过 —— 静默读未初始化，不是编译错误。
///
/// 实测形状（P16 轮转 phi 造的）：`tests/fixtures/opt/ssa_pipe_chain.c::chain_load4`
/// 与 `tests/fixtures/opt/ssa_pipe_rotation.c::rot_load4` 这类依赖载入链 ——
/// prologue 的最后一条 `_tbin_4 = a & 7` 只被 kernel 头的轮转 phi 读，丢掉后第一轮
/// `_tidx_9 = idx[_tbin_4]` 读到 0，-O2 得 372 而 gcc 四档都是 375。
fn read_by_phi(func: &Function, name: &str) -> bool {
    func.blocks.values().any(|bb| {
        bb.phis
            .iter()
            .any(|p| p.incoming.iter().any(|(src, _)| src.name() == name))
    })
}

/// 把一个临时值内联到使用点后, 该表达式**实际**读取的基名与内存/副作用需求。
struct ExpandedTemp {
    read_bases: HashSet<String>,
    reads_mem: bool,
    has_side_effect: bool,
    /// 展开链上最早一环在本块内的序号 (安全窗口的左边界)
    min_idx: usize,
}

/// 判定「把 `inst` 的结果内联到它唯一的使用点」是否保持语义。
///
/// 返回 `None` = 不安全 (调用方必须保留为独立赋值语句)。判定窗口是块内的
/// `[min_idx, use_pos)`:
///   * 窗口内不得写入表达式读到的任何**基名** —— 基名塌缩后这就是重算时机错;
///   * 表达式若读内存或自带副作用, 窗口内不得有任何写内存/调用/volatile 访问;
///   * 使用点必须与定义点同块 —— 跨块时中间可能经过任意循环回边/分支,
///     无法在「一条直线窗口」里证明稳定, 保守拒绝;
///   * 链式内联的操作数也必须同块且在窗口左界之前。
fn inline_window_safe(
    func: &Function,
    ssa_name: &str,
    def_bb: &str,
    def_idx: usize,
    inst: &Inst,
    def_pos: &HashMap<String, (String, usize)>,
    addr_base: &HashMap<String, String>,
    subst: &HashMap<String, Option<Expr>>,
    expanded: &HashMap<String, ExpandedTemp>,
) -> Option<ExpandedTemp> {
    let mut info = ExpandedTemp {
        read_bases: HashSet::new(),
        reads_mem: inst_reads_memory(&inst.kind),
        has_side_effect: inst_has_side_effect(&inst.kind),
        min_idx: def_idx,
    };
    for uvar in kind_operand_ssa_vars(&inst.kind) {
        let uname = uvar.name();
        if !matches!(subst.get(&uname), Some(Some(_))) {
            // 操作数保留为真实变量 (具名局部 / 未内联的临时值): 它按基名参与检查。
            info.read_bases.insert(uvar.base.clone());
            continue;
        }
        let (blk, idx) = def_pos.get(&uname)?.clone();
        if blk != def_bb {
            return None;
        }
        let inner = expanded.get(&uname)?;
        info.read_bases.extend(inner.read_bases.iter().cloned());
        info.reads_mem |= inner.reads_mem;
        info.has_side_effect |= inner.has_side_effect;
        info.min_idx = info.min_idx.min(inner.min_idx).min(idx);
    }
    let (u_bb, u_pos) = match find_use_site(func, ssa_name) {
        Some(p) => p,
        // 没有指令级消费者。这里必须**分两种**，不能一律放行：
        //   * 真的无人读 ⇒ 丢掉定义是安全的（也确实是原来的收益来源）；
        //   * 只被 phi 读 ⇒ **不能丢**（见 `read_by_phi`：重建把 phi 的 dest 别名到
        //     第一个 incoming，那条定义就是槽的初始化）。
        None => return if read_by_phi(func, ssa_name) { None } else { Some(info) },
    };
    if u_bb != def_bb || u_pos <= def_idx {
        return None;
    }
    let bb = func.blocks.get(def_bb)?;
    for other in bb.insts.iter().take(u_pos).skip(info.min_idx + 1) {
        if (info.reads_mem || info.has_side_effect) && inst_clobbers_memory(&other.kind) {
            return None;
        }
        if inst_written_bases(&other.kind, other.dest.as_ref(), addr_base)
            .iter()
            .any(|b| info.read_bases.contains(b))
        {
            return None;
        }
    }
    Some(info)
}

/// 「合成临时值」的判据: **先排除用户可见存储**, 再认已知的合成命名方案。
///
/// 两个方向都不能只用名字前缀:
///   * `_s` / `__d` / `_tbin` 是 C 合法的用户标识符。把它们当合成值会在重建时
///     内联掉它们的赋值、丢掉循环推进 —— 实测 `int _s=0; for(i<n) _s+=i;` 从 -O2
///     起算错。它们在 `original_locals ∪ params` 里, 所以上面那一律优先。
///   * 反向也不行: 本仓 pass 生成的临时名不必以 `_` 开头 (`dcc_t<序号>_xxx`)。
///   * 还有一类既不在 `original_locals` 也不属于合成命名方案的名字 —— C++ 后端的
///     短路求值结果变量 `dcc_land1` / `dcc_lor2` 就是这样的**具名局部** (它们不靠
///     `Decl::Var` 节点存在, 而靠重建按定义序合成声明), 把它们当合成值会让
///     `a ? b : c` 的结果丢失 (实测 cpp 夹具 `ternary=` 第二项变成 0.0000)。
///
/// 所以「是不是合成值」只能问「它匹配哪一套已登记的合成命名方案」, 并且用户可见
/// 声明集永远优先。`SsaVar::name()` 的分符改成 `.` 后, 版本名不会再与合成 base 撞名
/// (旧写法下 `int _tbin` 的第 2 版 = `_tbin_2` = `fresh("bin")` 的 base)。
pub(super) fn is_synthetic_temp_base(func: &Function, base: &str) -> bool {
    if func.original_locals.contains(base) {
        return false;
    }
    if func.params.iter().any(|p| p.base == base) {
        return false;
    }
    base.starts_with('_') || is_lower_ir_temp_base(base)
}

/// 本窗口的分级开关（`DCC_ADDR_FOLD`，默认开）：与 `DCC_BLOCKOPT` 同一条纪律 ——
/// 折叠类改动一旦削错，症状是「别处读到垃圾」，必须能用一个环境变量把它整体关掉
/// 复现，否则无法与同阶段的其他改动二分。
fn addr_fold_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("DCC_ADDR_FOLD")
            .map(|v| v.trim() != "0")
            .unwrap_or(true)
    })
}

/// 阶段2 尾项：地址临时值就地内联的**唯一豁免窗口**（真正的折叠动作在
/// `expr_rebuild::fold_direct_named_access`，这里只决定「敢不敢把 `AddrOf` 交出去」）。
///
/// `is_addr_producer` 的禁令对它成立的那些父级（按指针**值**做算术、再套一层取址、当
/// 下标基址）一概不变；这里只放开一种父子形态：`*(&sym) = v` / `x = *(&sym)` —— 地址
/// 就是对象自己的位置，中间没有任何指针值参与运算，摘掉这层 deref 后语义逐位相同。
///
/// 每个条件都必要：
///   * 生产者必须是最纯的 `AddrOf{base}`：`MemberAddr`/`IndexAddr` 带字段偏移与下标
///     步长，折叠后要重算落点，不属本窗口；
///   * `base` 不得是合成临时名 —— 它可能已被别的内联决策消掉声明，折进去会引用一个
///     从未声明的名字（后端按隐式 int 处理，指针/double 直接截断，见下方 kept_temp_bases
///     那段注释的同一条案底）；
///   * 不得被 phi 读（见 `read_by_phi`：那时它是槽的初始化，不是「无人读」）；
///   * 有使用点时必须是**紧邻的下一条**指令、且位置是那条 `Store`/`Load` 的地址操作数：
///     中间隔着任何指令，重建出的 AST 就把「取地址」与「访问」拆成两次，时机不再等价；
///   * 那条访存不得是 volatile（volatile 局部走的正是「`*(&v)` 保住访问次数」那条通路）。
fn addr_temp_folds_at_sole_memop(
    func: &Function,
    ssa_name: &str,
    def_bb: &str,
    def_idx: usize,
    kind: &InstKind,
) -> bool {
    if !addr_fold_enabled() {
        return false;
    }
    let InstKind::AddrOf { base } = kind else {
        return false;
    };
    if is_synthetic_temp_base(func, base) || read_by_phi(func, ssa_name) {
        return false;
    }
    let (u_bb, u_pos) = match find_use_site(func, ssa_name) {
        Some(p) => p,
        // 一次都没人读：`AddrOf` 不碰内存、无副作用（`inst_unremovable` 也不认它），
        // 放开等于扔掉那条 `lea` 和它的落槽写。
        None => return true,
    };
    if u_bb != def_bb || u_pos != def_idx + 1 {
        return false;
    }
    let Some(bb) = func.blocks.get(def_bb) else {
        return false;
    };
    // `u_pos == insts.len()`（用在 terminator 上）在这里自然取不到，一并拒掉。
    let Some(next) = bb.insts.get(u_pos) else {
        return false;
    };
    match &next.kind {
        InstKind::Store { e, is_volatile, .. } | InstKind::Load { e, is_volatile, .. } => {
            !*is_volatile && matches!(e, Operand::Var(v) if v.name() == ssa_name)
        },
        _ => false,
    }
}

/// SSA→AST 重建的内联计划。
pub(super) struct InlinePlan {
    /// SSA 名 -> 内联表达式 (`Some(None)` = 不可内联, 保留为独立赋值语句)
    pub(super) subst: HashMap<String, Option<Expr>>,
    /// 未内联因而必须以独立赋值发射、并且需要**局部声明**的 `_` 前缀临时基名
    pub(super) kept_temp_bases: OrderedNameSet,
}

/// 该指令的**结果**重建后是否是一个 `Expr::AddrOf{ .. }`（即一个左值地址，而非载入值）。
/// 只有这三类会：`AddrOf`（具名对象基址）、`MemberAddr`（`&obj.field`）、`IndexAddr`
/// （`&arr[idx]`）。它们的值是「地址」，一旦被就地内联进父表达式，父级若按指针**值**再
/// 参与算术（减法求元素差、作为另一条取址/下标的基址），重建 AST 会把这层
/// `AddrOf{ Index }` 连同步长语义一起搬过去 —— 实测把单用例地址临时值内联进指针减法会
/// 把结果算成 0 / 负数 / 巨大垃圾，内联进 `IndexAddr.arr` 更会退化成段错误的嵌套取址。
/// 故地址类临时值一律**不内联**，保留成真实变量（值不变，代价只是多一个栈槽）。
fn is_addr_producer(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::AddrOf { .. } | InstKind::MemberAddr { .. } | InstKind::IndexAddr { .. }
    )
}

/// 把 SSA→AST 重建「单次使用临时值就地内联」的决策算一遍, 产出 `subst` 表。
///
/// 必须独立成函数并在**生成局部声明之前**调用: 被拒绝内联的临时值会退化成一条
/// 对 `_xxx` 变量的赋值语句, 于是那个名字必须进局部声明表 (否则后端按隐式 int
/// 处理, 指针/double 临时值直接被截断) —— 声明序因此依赖内联决策。
pub(super) fn plan_inlined_temps(func: &Function) -> InlinePlan {
    // 收集所有被用作操作数的 SSA 变量
    let mut used_ssa: HashSet<String> = HashSet::new();
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            collect_op_uses(&inst.kind, &mut used_ssa);
        }
        match &bb.term {
            Terminator::Ret { val: Some(o) } => collect_op_uses_op(o, &mut used_ssa),
            Terminator::Branch { cond, .. } => collect_op_uses_op(cond, &mut used_ssa),
            Terminator::SwitchTable { idx, .. } => collect_op_uses_op(idx, &mut used_ssa),
            _ => {},
        }
    }

    // 定义位置索引 (SSA 名 -> 块/序号) 与 `AddrOf` 目标基名索引。
    // 按 `block_order` 创建序遍历, 保证同一名字多次定义时首个胜出且与哈希序无关。
    let mut def_pos: HashMap<String, (String, usize)> = HashMap::new();
    let mut addr_base: HashMap<String, String> = HashMap::new();
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for (idx, inst) in bb.insts.iter().enumerate() {
            let Some(d) = &inst.dest else { continue };
            def_pos.entry(d.name()).or_insert((bb_name.clone(), idx));
            if let InstKind::AddrOf { base } = &inst.kind {
                addr_base.entry(d.name()).or_insert_with(|| base.clone());
            }
        }
    }

    let mut subst: HashMap<String, Option<Expr>> = HashMap::new();
    let mut kept_temp_bases = OrderedNameSet::new();
    let mut expanded: HashMap<String, ExpandedTemp> = HashMap::new();

    // 对每个 SSA 定义: 如果是临时变量且只被使用一次, 标记为内联
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for (idx, inst) in bb.insts.iter().enumerate() {
            let Some(d) = &inst.dest else { continue };
            let ssa_name = d.name();
            // 「合成临时值」的判定必须覆盖生产端的**全部**命名方案 (`_xxx` 与
            // `dcc_t<序号>_xxx` 都在内): 漏掉一类不是「少内联一次」, 而是**静默错值** ——
            // `rebuild_cycle` 在能提出循环条件时会整体丢弃节点头块的普通指令 (那里只有
            // `cond_expr.is_none()` 才 emit), 前提是条件操作数已被本表 (subst) 内联成
            // 表达式; 判定失效后 `while (i < 3)` 的这次比较连同写入一起消失, 只剩
            // `Stmt::While{cond: Ident "dcc_t1__tmp"}` 读一个从未赋值的局部 (0),
            // 循环体一次都不执行。但判定也不能把**用户变量**当临时值 (前缀启发式的错),
            // 所以用 `is_synthetic_temp_base`: 它只信「局部声明集 + 参数」。
            let is_temp = is_synthetic_temp_base(func, &d.base);
            let use_count = used_ssa.get(&ssa_name).map_or(0, |_| {
                let mut count = 0;
                for b in func.blocks.values() {
                    for i in &b.insts {
                        count += op_count_in_kind(&i.kind, &ssa_name);
                    }
                    match &b.term {
                        Terminator::Ret { val: Some(o) } => {
                            count += op_count_in_op(o, &ssa_name);
                        },
                        Terminator::Branch { cond, .. } => {
                            count += op_count_in_op(cond, &ssa_name);
                        },
                        Terminator::SwitchTable { idx, .. } => {
                            count += op_count_in_op(idx, &ssa_name);
                        },
                        _ => {},
                    }
                }
                count
            });
            // 有副作用的临时结果 (如返回值被丢弃的 Call) 在
            // use_count==0 时不允许内联: 表达式存入 subst 后没有任何
            // 使用点消费, 重建时会被跳过, 副作用随之丢失. volatile 访问
            // (含未被读的 volatile 局部) 同理 —— 那次访问本身就是结果.
            let side_effect_unused = use_count == 0 && inst_unremovable(&inst.kind);
            // C++ 零成本 EH: 受保护调用与落地垫序言**永不内联**。
            // 内联等于把定义点丢掉、在使用处重算: 前者会让 LSDA 里登记
            // 的地址区间指向一条不存在的调用指令 (异常找不到帧), 后者会
            // 让落地垫序言不再是一条独立语句 (rax/rdx 被覆盖)。
            let is_eh_pinned =
                matches!(inst.kind, InstKind::Invoke { .. } | InstKind::EhLandingPad { .. });
            // 地址类临时值一律不内联：`AddrOf`/`MemberAddr`/`IndexAddr` 的结果重建后是
            // `Expr::AddrOf{ .. }`，把它就地塞进任一父表达式都会让地址语义漂移（父级按
            // 指针值重算步长 / 再包一层取址）。保留成真实临时变量后，消费方走 codegen 的
            // 「载入指针值再按其类型偏移」分支，语义无歧义。代价只是多一个栈槽，不改变值。
            // 唯一豁免见 `addr_temp_folds_at_sole_memop`（P13 阶段2 尾项）：紧邻的
            // `*(&sym)=v` / `x=*(&sym)` 还原成对 `sym` 的直接访问，地址临时值连同它的
            // 栈槽与那条落槽写一起消失。
            let addr_operand_pinned = is_addr_producer(&inst.kind)
                && !addr_temp_folds_at_sole_memop(func, &ssa_name, bb_name, idx, &inst.kind);
            let mut chain: Option<ExpandedTemp> = None;
            let inlineable = is_temp
                && use_count <= 1
                && !side_effect_unused
                && !is_eh_pinned
                && !addr_operand_pinned
                && {
                    // SSA→AST 重建把 `n_1`/`n_2` 塌缩回同一个变量 `n`, 于是内联
                    // 「在使用点重算表达式」会把对 `n` 的读**搬到中途赋值之后**。
                    // 真 SSA 里无害, 塌缩后就是静默错值: 调度后
                    // `_tbin = 2*n; _tupd = n-1; n = _tupd; s = _tbin + s`
                    // 内联成 `s = (2*n) + s`, 乘的是**已自减**的 `n` —— 实测
                    // `int s=0; while(n>0){int step=2; s+=step*n; n--;}` 的
                    // `a(4)` 在 -O3 返回 6 而不是 12。因此内联必须过窗口判定。
                    match inline_window_safe(
                        func, &ssa_name, bb_name, idx, inst, &def_pos, &addr_base, &subst,
                        &expanded,
                    ) {
                        Some(info) => {
                            chain = Some(info);
                            true
                        },
                        None => false,
                    }
                };
            if inlineable {
                let resolved =
                    inst_to_expr_resolved(&inst.kind, &subst, &inst.span, func, inst.dest.as_ref());
                // （这里曾加过一道「展开表达式的类型必须等于 dest 类型才内联」的守卫；
                // 实测它不是必需的：-O2 丢 `__seg_gs` 的真因在 `ast_gvn::coalesce_block`
                // 的类型判据（见那里的说明），修完之后它只会把 C++ 降级里大量合法的
                // 类型转换拷贝阻挡在链式内联之外，实测表现为 `g + 6 - g` 算错。)
                subst.insert(ssa_name.clone(), Some(resolved));
                if let Some(info) = chain {
                    expanded.insert(ssa_name, info);
                }
            } else {
                subst.insert(ssa_name, None);
                // 未内联的合成临时值会发射成对 `_xxx`/`dcc_tX_xxx` 的赋值: 必须有
                // 声明, 否则后端按隐式 int 处理。用户自己的 `_s` 不在这里 (它已在
                // `original_locals` 里被声明), 用 is_temp 区分。
                if is_temp && !is_pass_hint_inst(&inst.kind) {
                    kept_temp_bases.insert(d.base.clone());
                }
            }
        }
    }

    InlinePlan {
        subst,
        kept_temp_bases,
    }
}

/// `&&` / `||` 的**右**操作数只在左操作数为真 (假) 时才该求值。SSA 通道不为短路求值造块:
/// 整个条件是一棵交给重建的表达式树, 于是树里每一个值都必须**内联**进那棵树 —— 否则它就
/// 退化成块里的一条独立赋值语句 (`plan_inlined_temps` 给的 `subst[name] == Some(None)`),
/// 而独立语句按块序发射, 发在带守卫的那条分支**之前**, 那次被守卫保护的求值就提前发生了:
/// 轻则读到未初始化/过期的值而走进错误分支, 重则直接打空段错误。
///
/// 实测 (`tests/fixtures/opt/lazy_guard_and.c::land_addr`, -O2/-O3 SIGSEGV, 而 -O0/-O1
/// 与 gcc 四档都对): `if( pl && pl->nExpr > db->aLimit[5] && !nested )` 里 `db->aLimit`
/// 的基址先落进 `-0x58(%r13)`、比较结果落进 `-0x60(%r13)`, 而 `test %rax,%rax / je`
/// (pl 的非空判断) 直到它们**之后**才发, `pl==0` 时 `movslq (%rax)` 打空。同一条式子改成
/// `> 5`、改成 `aLimit[5] < ...`(操作数互换)、改成嵌套 if 都不犯 —— 犯不犯取决于内联窗口
/// 里是否恰好卡进一条 `MemberAddr` 的定义 (`is_addr_producer` 的政策让那种临时值永不内联),
/// 那是排布巧合, 不是语义, 所以不能靠调窗口判据的松紧来解决。
///
/// 判据沿「被守卫的名字 → 其定义的操作数」传递: 定义已内联的继续往下查 (它此刻在同一棵
/// 被守卫的树里), 定义被落地的就是本判据要拦的。名字不在 `subst` 里 (参数 / 具名局部 /
/// phi 别名) 视为安全: 它们读的是真实槽, 块序保证写在前、读在后。
///
/// 三元 `?:` 不走这条路 —— 降级阶段就把含 `Expr::Cond` 的表达式判为不支持而整函数退出
/// SSA (实测 `p ? p->x : 0` 的函数不进 SSA), 所以这里只认 `&&` / `||`。
///
/// 判据**保守**: 落地一次就整体退出, 不去判「提前的那次求值其实没有副作用」。少退出的
/// 代价是丢优化 (与 SSA 关闭同一产物), 多退出的代价是错值 —— 两边不对称。
pub(super) fn lazy_operand_landed(func: &Function, subst: &HashMap<String, Option<Expr>>) -> bool {
    // 名字 → 该定义的操作数名字集。首个定义胜出, 与 `plan_inlined_temps` 的 `def_pos`
    // 同一条规则: 内联计划看见的是哪一个定义, 这里就必须顺着同一个定义往下走。
    let mut def_ops: HashMap<String, Vec<String>> = HashMap::new();
    let mut guarded: Vec<String> = Vec::new();
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                def_ops.entry(d.name()).or_insert_with(|| {
                    let mut names = HashSet::new();
                    collect_op_uses(&inst.kind, &mut names);
                    names.into_iter().collect()
                });
            }
            if let InstKind::Bin { op, r, .. } = &inst.kind {
                if matches!(op, BinOp::AndAnd | BinOp::OrOr) {
                    if let Operand::Var(v) = r {
                        guarded.push(v.name());
                    }
                }
            }
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(name) = guarded.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        match subst.get(&name) {
            // 不是指令定义出来的名字: 读真实槽, 块序已保证先后。
            None => {},
            // 已内联 ⇒ 值仍在被守卫的树里, 继续查这棵树自己的操作数。
            Some(Some(_)) => {
                if let Some(ops) = def_ops.get(&name) {
                    guarded.extend(ops.iter().cloned());
                }
            },
            // 被落成守卫之外的一条独立语句 ⇒ 求值时机已经变了。
            Some(None) => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Type;
    use crate::diagnostics::Span;
    use crate::ssa::{BasicBlock, Phi, SsaVar};

    fn v(base: &str, version: u32) -> SsaVar {
        SsaVar {
            base: base.to_string(),
            version,
            ty: Type::Int,
            span: Span::point("", 0, 0),
        }
    }
    fn blk(name: &str, phis: Vec<Phi>) -> BasicBlock {
        BasicBlock {
            name: name.to_string(),
            phis,
            insts: Vec::new(),
            term: Terminator::Ret { val: None },
            preds: Vec::new(),
            succs: Vec::new(),
        }
    }
    fn func(phis: Vec<Phi>) -> Function {
        let mut f = Function::new("f".to_string(), Type::Int, Span::point("", 0, 0));
        f.blocks.insert("head".to_string(), blk("head", phis));
        f.block_order = vec!["head".to_string()];
        f
    }

    /// phi 的入边**是**一次读。`find_use_site`/`use_count` 看不到它，而 `rebuild_cycle`
    /// 把头块 phi 的 dest 别名到第一个 incoming，所以那条定义就是槽的初始化。
    /// 把「只被 phi 读」当成「无人读」⇒ 定义被内联丢掉 ⇒ 槽从未被写 ⇒ 静默读未初始化
    /// （实测 -O2 得 372 而 gcc 四档 375，见 `read_by_phi` 的注释与
    /// `tests/integration/p16_rotation.rs` 的槽初始化判据）。
    #[test]
    fn a_phi_incoming_counts_as_a_read() {
        let f = func(vec![Phi {
            dest: v("_tbin", 2),
            incoming: vec![
                (v("_tbin", 1), "pre".to_string()),
                (v("_tbin", 3), "head".to_string()),
            ],
        }]);
        assert!(read_by_phi(&f, "_tbin.1"), "phi 的 incoming 必须算一次读");
        assert!(read_by_phi(&f, "_tbin.3"), "latch 侧那份同样是 phi 的入边");
        assert!(!read_by_phi(&f, "_tbin.2"), "phi 的 dest 不是它自己的入边");
        assert!(
            !read_by_phi(&f, "nobody.reads.me"),
            "无人读就是无人读（这条捷径是本 pass 的收益来源）"
        );
        assert!(find_use_site(&f, "_tbin.1").is_none(), "本例正是要暴露「指令侧无人读」这一面");
    }

    /// 反向：真的无人读时 `read_by_phi` 给出 `false`，上面那条「可以内联」的捷径仍然成立
    /// （它是这条 pass 的收益来源，不能一并关掉）。
    #[test]
    fn an_unread_name_is_still_recognised_as_unread() {
        let f = func(vec![Phi {
            dest: v("a", 2),
            incoming: vec![
                (v("a", 1), "pre".to_string()),
                (v("a", 3), "head".to_string()),
            ],
        }]);
        assert!(!read_by_phi(&f, "nobody.reads.me"));
        assert!(find_use_site(&f, "nobody.reads.me").is_none());
        assert!(!read_by_phi(&f, "a.9"), "不存在的版本当然没被读");
        assert!(read_by_phi(&f, "a.1"), "preheader 侧那份是被读的");
    }
}
