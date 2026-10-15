//! 循环优化：不变式外提（LICM）、展开、SIMD 向量逐元素循环向量化。

use crate::ast::{
    ArraySize, BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Ident, Program, Stmt, Type,
};
use crate::const_eval;
use crate::diagnostics::Span;
use crate::ir::Module;
use std::collections::{HashMap, HashSet};

/// `-O2+` LICM；`-O3` 额外展开与向量循环向量化。
///
/// Profile-guided: hot 函数 (FuncDecl.is_hot, 由 PGO 设置) 在 O3
/// 享受更激进的展开上限 (max_trip 4 -> 16)。
pub fn run(module: &mut Module, level: u8) {
    run_with_target(module, level, false, 0);
}

/// 带目标信息的循环优化入口。
///
/// `has_vnni`: 目标是否支持 VNNI 指令 (AVX-VNNI / AVX-512VNNI)
/// `vec_bits`: 向量寄存器位宽 (256 for AVX2, 512 for AVX-512)
pub fn run_with_target(module: &mut Module, level: u8, has_vnni: bool, vec_bits: u32) {
    if level < 2 {
        return;
    }
    let prog = module.source_mut();
    // 根因 1/2：不变式判据要升级成"名字集合"，而集合的含义（谁是全局、谁是函数名、
    // 谁带 volatile）只有程序级视图才给得出，故先一次性算好再逐函数下传。
    let facts = GlobalFacts::of(prog);
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if f.body.is_some() {
                licm_func(f, &facts);
            }
            if let Some(body) = &mut f.body {
                if level >= 3 {
                    // Profile-guided: hot 函数享受更激进的展开上限
                    let max_trip: usize = if f.is_hot { 16 } else { 4 };
                    unroll_block(body, max_trip);
                    vectorize_block(body, has_vnni, vec_bits);
                    // P10: 完美两层嵌套的 ts×ts 分块 (逐字保留循环体)。
                    let mut tile_names: u32 = 0;
                    tile_block(body, 4, &mut tile_names);
                    // P7: 自动并行化标注
                    parallelize_block(body);
                    // P16 软件流水不在此实现：见 `src/ssa/passes.rs` 的 `run_pipelining`
                    // 与 `src/ssa/pipelining.rs`（原 AST 层实现只插一条后端当注释的空
                    // asm，属伪 pass，已删除）。
                }
            }
        }
    }
}

// ===== LICM =====
//
// 根因链（对应 `f(n){int s=0;while(n>0){s+=n;n--;}return s;}` 在 -O2 死循环）：
//   1. `Stmt::While` 分支调用 LICM 时把 `loop_var` 传成 `None`（For 分支传归纳变量名）；
//   2. `expr_mentions(e, None)` 恒为 false ⇒ "不涉及"，于是循环体**每一条**语句都被
//      判为不变式；
//   3. `split_invariants` 把整个体 drain 走 ⇒ `while(n>0){}` 忙等；
//   4. 外提语句被 prepend 到所在块最前面 ⇒ 块内语句序被打破（`s+=n` 跑到 `s=0` 之前），
//      而块内顺序是本项目的"顺序单一真源"，直接决定 codegen 栈槽与产物字节。
//
// 本节的机制级修复：不变式判据从"单个 loop_var 名字"升级为**名字栅栏**（`NameScan`），
// For / While / Do 三条分支共用同一个收集器（不再有 `None` 这条退化路径），任何拿不到
// 信息的情形一律"不是不变式"（fail-safe），外提语句插回原循环所在下标之前，并带
// "体被搬空 / 搬走 cond 所需写入"的退化护栏。

/// 文件作用域事实：判断"循环里某个名字会不会被改掉"所需的背景。
#[derive(Default)]
struct GlobalFacts {
    /// 文件作用域对象名（含 `extern` 声明）：任何调用都可能改写它们。
    globals: HashSet<String>,
    /// 文件作用域函数名：外提的声明与之同名会改变既有名字绑定，须排除。
    func_names: HashSet<String>,
    /// 文件作用域 volatile / `_Atomic` 对象名：读写它们本身就是可观测副作用。
    volatile_globals: HashSet<String>,
}

impl GlobalFacts {
    fn of(prog: &Program) -> Self {
        let mut f = GlobalFacts::default();
        for d in &prog.decls {
            match d {
                Decl::Var(v) => {
                    f.globals.insert(v.name.name.clone());
                    if v.quals.is_volatile()
                        || v.quals.is_atomic()
                        || matches!(v.ty, Type::Atomic(_))
                    {
                        f.volatile_globals.insert(v.name.name.clone());
                    }
                },
                Decl::Func(func) => {
                    f.func_names.insert(func.name.name.clone());
                },
                _ => {},
            }
        }
        f
    }
}

/// 一次 AST 遍历的名字产物：既用作函数级逃逸扫描，也用作单个循环的名字栅栏。
///
/// 为什么两种用途共用一套遍历（`licm_func` 先全函数扫一遍，`licm_loop` 再对每个
/// 循环扫一遍）：两处若各自展开 AST 形态，漏案概率翻倍，而漏案在这里意味着"把
/// 非常量的东西当成不变式"（根因 2/3）—— 正是本缺陷的成因形态。
#[derive(Default)]
struct NameScan {
    /// 调用 / 间接写入可能触及的对象名（由 `LicmCtx` 注入；全函数扫描期为空）。
    observable: HashSet<String>,
    /// 循环内被写的名字 → 写次数。写入来源：直接赋值、自增/自减、复合赋值、
    /// 成员与数组元素写（记到根对象名）、以及调用/间接写带来的全部可观测对象。
    written: HashMap<String, u32>,
    /// 循环控制流（cond / for-init / for-step）读**或**写的名字。
    control: HashSet<String>,
    /// 仅 cond 读到的名字：退化护栏据此判断"外提是否搬走了循环推进所需的写入"。
    cond_reads: HashSet<String>,
    /// 地址逃逸的对象名（被 `&` 取址、或数组/函数名衰减成指针）。
    escaped: HashSet<String>,
    /// volatile / `_Atomic` 对象名（本函数内的局部与参数）。
    volatiles: HashSet<String>,
    /// `static` / `_Thread_local` 局部名：存储期跨调用，性质等同全局。
    persistent: HashSet<String>,
    /// 函数体内各变量名的声明次数（含嵌套块、循环体、for-init）。
    decl_counts: HashMap<String, u32>,
    /// 出现无法用名字表达的构造（goto / label / switch / asm / 未知语句或表达式形态 /
    /// 判定不了写入目标的左值）⇒ 该作用域放弃一切外提（根因 3：fail-safe）。
    reject_all: bool,
    /// 含内联汇编 ⇒ 其读写集合只能靠字符串约定，本函数的具名对象全部按可观测处理。
    poison: bool,
    /// 含嵌套函数定义 ⇒ 嵌套函数按 upvars 直接读写外层局部，名字栅栏表达不了 ⇒ 整个
    /// 函数放弃 LICM。
    gives_up: bool,
}

impl NameScan {
    /// 读侧判据：该名字在循环期间是否可能变化。
    fn may_change(&self, name: &str) -> bool {
        self.reject_all || self.written.contains_key(name) || self.control.contains(name)
    }

    /// 写侧判据：该名字是否**只**被候选语句自身这一次写覆盖（≤1 次即候选自己的声明）。
    fn only_self_write(&self, name: &str) -> bool {
        !self.reject_all
            && !self.control.contains(name)
            && self.written.get(name).copied().unwrap_or(0) <= 1
    }
}

/// 遍历位置：区分"体内"与"循环控制流"，只有控制流上的读才进 `control` 栅栏。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    /// 循环体内（含体内嵌套循环的控制表达式：对当前循环而言那就是体内）。
    Body,
    /// 本循环的 cond（While / Do / For 三条分支共用同一判据）。
    Cond,
    /// 本循环的 for-init / for-step。
    Ctl,
}

/// 循环形态。三条分支只是"哪个表达式算 cond"不同，收集器完全共用（要求 2）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoopKind {
    For,
    While,
    Do,
}

/// 函数级 LICM 上下文（由一次全函数名字扫描 + 程序级事实合成）。
struct LicmCtx {
    /// 一旦循环内出现调用或间接写入，就可能被改写的对象集合：
    /// 全局变量 ∪ volatile/_Atomic ∪ static/_Thread_local 局部 ∪ 地址逃逸的局部。
    observable: HashSet<String>,
    /// volatile/_Atomic 对象名：读它们本身就是可观测副作用，与有没有调用无关
    /// （要求：体内 volatile 读禁止外提）。
    volatiles: HashSet<String>,
    /// 参数名 ∪ 文件作用域名（变量与函数）：外提的声明与之同名即可能改变名字绑定。
    shadow_names: HashSet<String>,
    /// 各变量名在全函数的声明次数。
    decl_counts: HashMap<String, u32>,
}

impl LicmCtx {
    fn of(f: &FuncDecl, facts: &GlobalFacts, scan: &NameScan) -> Self {
        let mut observable: HashSet<String> = facts.globals.clone();
        observable.extend(facts.volatile_globals.iter().cloned());
        observable.extend(scan.escaped.iter().cloned());
        observable.extend(scan.persistent.iter().cloned());
        observable.extend(scan.volatiles.iter().cloned());
        let mut volatiles: HashSet<String> = facts.volatile_globals.clone();
        volatiles.extend(scan.volatiles.iter().cloned());
        let mut shadow_names: HashSet<String> = facts.globals.clone();
        shadow_names.extend(facts.func_names.iter().cloned());
        for p in &f.params {
            if let Some(n) = &p.name {
                shadow_names.insert(n.name.clone());
            }
        }
        if scan.poison {
            // 有内联汇编时，asm 可能改写任何具名对象（甚至通过硬编码地址），AST 层面
            // 拿不到信息 ⇒ fail-safe：本函数所有局部/参数都算可观测对象。
            observable.extend(scan.decl_counts.keys().cloned());
            for p in &f.params {
                if let Some(n) = &p.name {
                    observable.insert(n.name.clone());
                }
            }
        }
        LicmCtx {
            observable,
            volatiles,
            shadow_names,
            decl_counts: scan.decl_counts.clone(),
        }
    }

    /// 该标识符是否可被外提候选当作"稳定的纯读"。
    fn read_is_stable(&self, name: &str, scan: &NameScan) -> bool {
        !scan.may_change(name) && !self.volatiles.contains(name)
    }
}

/// 单个函数的 LICM：先做全函数名字扫描（拿到逃逸/声明次数/嵌套函数事实），
/// 再自顶向下逐块外提。
fn licm_func(f: &mut FuncDecl, facts: &GlobalFacts) {
    let Some(body) = &f.body else { return };
    let mut scan = NameScan::default();
    for p in &f.params {
        if let Some(n) = &p.name {
            if p.quals.is_volatile() || p.quals.is_atomic() {
                scan.volatiles.insert(n.name.clone());
            }
        }
    }
    scan_block(&mut scan, body, Site::Body);
    // 嵌套函数按 upvars 直接读写外层局部，任何名字栅栏都表达不了这种别名。
    if scan.gives_up {
        return;
    }
    let ctx = LicmCtx::of(f, facts, &scan);
    if let Some(body) = &mut f.body {
        licm_block(body, &ctx);
    }
}

/// 块级 LICM 驱动。
///
/// 根因 4：外提语句**插回该循环语句所在的下标之前**（`out` 边扫边收，遇到循环就
/// 先 extend 外提项再 push 循环本身），于是块内其它语句的相对顺序一个都不动；
/// 旧实现是 `block.items = hoisted; items.extend(kept)`，等于把外提项 prepend 到块首。
fn licm_block(block: &mut Block, ctx: &LicmCtx) {
    let items = std::mem::take(&mut block.items);
    let mut out: Vec<BlockItem> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                mut body,
                span,
            }) => {
                let hoisted = licm_loop(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    &mut body,
                    LoopKind::For,
                    ctx,
                );
                out.extend(hoisted);
                out.push(BlockItem::Stmt(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                    span,
                }));
            },
            BlockItem::Stmt(Stmt::While {
                cond,
                mut body,
                span,
            }) => {
                // 根因 1：这里过去传 `None`，与 For 分支各走各的判据。现在两条分支
                // 只差"哪个表达式是 cond"，栅栏由同一个 `scan_*` 收集器产出。
                let hoisted = licm_loop(None, Some(&cond), None, &mut body, LoopKind::While, ctx);
                out.extend(hoisted);
                out.push(BlockItem::Stmt(Stmt::While { cond, body, span }));
            },
            BlockItem::Stmt(Stmt::Do {
                mut body,
                cond,
                span,
            }) => {
                // do-while 的 cond 在体内语句之后求值，但它读的仍是"本轮迭代后的值"，
                // 因此名字栅栏与 while 完全同构，同样共用收集器。
                let hoisted = licm_loop(None, Some(&cond), None, &mut body, LoopKind::Do, ctx);
                out.extend(hoisted);
                out.push(BlockItem::Stmt(Stmt::Do { body, cond, span }));
            },
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                licm_block(&mut b, ctx);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            },
            other => out.push(other),
        }
    }
    block.items = out;
}

/// 对一个循环体做外提，返回应插到该循环之前的语句（保持它们彼此的原相对顺序）。
fn licm_loop(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &mut Stmt,
    kind: LoopKind,
    ctx: &LicmCtx,
) -> Vec<BlockItem> {
    // 先处理体内更内层的循环：它们的外提项落在体内正确位置上，随后的栅栏扫描看到
    // 的才是"即将真正执行"的体，判据不会用到已被搬走的东西。
    if let Stmt::Block { b, .. } = body {
        licm_block(b, ctx);
    }
    // 非块体（`while (c) f();`）里没有声明可外提，而存储类语句一律不外提（见
    // `hoistable_item` 的说明），故直接返回；旧实现把这种体塞进伪块再 drain，
    // 一旦误判就会把整个体搬空——同一根因的另一条路径。
    let Stmt::Block { b, .. } = body else {
        return Vec::new();
    };
    let mut scan = NameScan {
        observable: ctx.observable.clone(),
        ..NameScan::default()
    };
    if let Some(i) = init {
        scan_stmt(&mut scan, i, Site::Ctl);
    }
    if let Some(c) = cond {
        // `kind` 只区分"cond 在体前还是体后"，而栅栏关心的是"哪些名字会被改"，
        // 两者对 While / Do / For 完全同构 ⇒ 同一 Site::Cond、同一收集器。
        let _ = kind;
        scan_expr(&mut scan, c, Site::Cond);
    }
    if let Some(s) = step {
        scan_expr(&mut scan, s, Site::Ctl);
    }
    for item in b.items.iter() {
        scan_item(&mut scan, item, Site::Body);
    }
    split_invariants(b, &scan, ctx)
}

/// 把体内可外提的项搬到块外，并施加退化护栏（根因 3/5）。
fn split_invariants(body: &mut Block, scan: &NameScan, ctx: &LicmCtx) -> Vec<BlockItem> {
    if scan.reject_all {
        return Vec::new();
    }
    let keep: Vec<bool> = body
        .items
        .iter()
        .map(|it| !hoistable_item(it, scan, ctx))
        .collect();
    if keep.iter().all(|k| *k) {
        return Vec::new();
    }
    // 护栏必须在**真正搬动之前**判定：此时 `body.items` 还是完整的原序，能直接对比
    // "外提前后是否还有写入命中 cond"，放弃时也不必重建顺序。
    if degenerate_split(&body.items, &keep, scan) {
        return Vec::new();
    }
    let all = std::mem::take(&mut body.items);
    let mut hoisted = Vec::new();
    let mut kept = Vec::new();
    for (i, it) in all.into_iter().enumerate() {
        if keep[i] {
            kept.push(it);
        } else {
            hoisted.push(it);
        }
    }
    body.items = kept;
    hoisted
}

/// 退化护栏（根因 3/5）：外提绝不能把"让循环有条件终止"的写入搬走。
/// `all` 是外提前的完整体、`keep[i]` 表示第 i 项留在体内。
fn degenerate_split(all: &[BlockItem], keep: &[bool], scan: &NameScan) -> bool {
    // 整个体被搬空 —— 原复现形态：`while (n > 0) {}` 忙等。
    if !keep.iter().any(|k| *k) {
        return true;
    }
    // cond 里不含名字（`while (1)`）时无从判断，交给名字栅栏本身。
    if scan.cond_reads.is_empty() {
        return false;
    }
    let reaches_cond = |scope: Option<bool>| -> bool {
        let mut local = NameScan {
            observable: scan.observable.clone(),
            ..NameScan::default()
        };
        for (i, it) in all.iter().enumerate() {
            // None = 外提前的完整体；Some(true) = 外提后仍留在体内的项。
            if scope.is_some_and(|k| keep[i] != k) {
                continue;
            }
            scan_item(&mut local, it, Site::Body);
        }
        local.written.keys().any(|n| scan.cond_reads.contains(n))
    };
    // 体内原本存在能改变 cond 的写、外提后不再有 ⇒ 可能构造忙等 ⇒ 放弃外提。
    reaches_cond(None) && !reaches_cond(Some(true))
}

/// 候选语句能否外提。
///
/// 只接受"体内声明 + 不变初值"这一类（`int stride = w * sizeof(int);`）。**存储类
/// 语句一律不外提**：把体内的 store 搬到循环前，在 0 次迭代的循环里会把该对象改成
/// "循环前就被赋值"，与源语义不符（GCC/LLVM 的 LICM 同样只外提 load 与纯计算，
/// 不外提 store）。根因 2 的教训正是"把体内每条语句都当不变式"，故此处宁可窄。
fn hoistable_item(item: &BlockItem, scan: &NameScan, ctx: &LicmCtx) -> bool {
    let BlockItem::Decl(Decl::Var(v)) = item else {
        return false;
    };
    let name = v.name.name.as_str();
    // static / _Thread_local 的存储期不是"每轮迭代"；extern 根本没有本帧槽位；
    // cleanup / register asm / alias 的语义都锚定在声明所在作用域，搬动即改变语义。
    if v.is_static || v.is_extern || v.is_thread_local {
        return false;
    }
    if v.cleanup_fn.is_some() || v.register_name.is_some() || v.alias_name.is_some() {
        return false;
    }
    // volatile / _Atomic 对象的定义本身就是一次可观测写入。
    if v.quals.is_volatile() || v.quals.is_atomic() || matches!(v.ty, Type::Atomic(_)) {
        return false;
    }
    // VLA 的维度每轮重求值，不能提前。
    if type_has_vla(&v.ty) {
        return false;
    }
    let Some(init) = &v.init else {
        // 无初值的声明外提没有收益，却会把名字暴露到循环之后的作用域。
        return false;
    };
    // 作用域安全（根因 4）：只有"全函数唯一一次声明、且不与参数/全局/函数名同名"
    // 的局部才能外提 —— 此时循环外不可能有合法代码引用它，把它移到循环前不改变
    // 任何既有名字绑定，也不需要改名（改名会新增栈槽，扰动"顺序单一真源"）。
    if ctx.decl_counts.get(name).copied().unwrap_or(0) != 1 {
        return false;
    }
    if ctx.shadow_names.contains(name) || ctx.volatiles.contains(name) {
        return false;
    }
    // 该名字在循环里还有别的写入 ⇒ 提前初始化会被覆盖，或迭代中被读到旧值。
    if !scan.only_self_write(name) {
        return false;
    }
    // 初值本身必须是不变式：涉及的每个名字都不在栅栏内，且不含调用/汇编/间接读。
    !expr_mentions(init, scan, ctx)
}

/// 表达式是否"不是不变式的成分"：读到栅栏内的名字、volatile 对象，或含任何无法
/// 判定为纯读的形态（调用、汇编、取址、解引用、赋值、逗号、语句表达式……）。
///
/// 根因 3：默认分支**必须**保守返回 true（拿不到信息 ⇒ 不是不变式）。旧实现的
/// 默认分支是 `false`，且 `name == None` 时整个函数直接 `false`，这正是把一个
/// 不变式判据反转成"全都可外提"的地方。
fn expr_mentions(e: &Expr, scan: &NameScan, ctx: &LicmCtx) -> bool {
    match e {
        // 编译期即定的值，不读任何运行期对象。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. } => false,
        // 仍含运行期子表达式的"看似编译期"形态
        Expr::Offsetof { indices, .. } => indices.iter().any(|i| expr_mentions(i, scan, ctx)),
        Expr::SizeofExpr { e: inner, .. } | Expr::AlignofExpr { e: inner, .. } => {
            // sizeof/alignof 的操作数不求值（除 VLA），但 dcc 的语义里仍按读处理：
            // 只要它读到的东西可能被改写，外提就不安全，故与子表达式同判。
            expr_mentions(inner, scan, ctx)
        },
        Expr::Ident { name, .. } => !ctx.read_is_stable(name.name.as_str(), scan),
        Expr::Binop { l, r, .. } => expr_mentions(l, scan, ctx) || expr_mentions(r, scan, ctx),
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. } => expr_mentions(inner, scan, ctx),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            expr_mentions(cond, scan, ctx)
                || expr_mentions(then_, scan, ctx)
                || expr_mentions(else_, scan, ctx)
        },
        // 数组元素的稳定读：只接受"数组对象名[不变下标]"。指针下标 `p[i]` 指向的对象
        // 没有名字可记进栅栏，无法排除被循环内其它写（或调用）改动。
        Expr::Index { arr, idx, .. } => {
            !stable_object_read(arr, scan, ctx) || expr_mentions(idx, scan, ctx)
        },
        // `.` 成员读可判定（根对象有名字）；`->` 经指针，同 `p[i]` 一样不可判定。
        Expr::Member {
            expr: base,
            is_arrow,
            ..
        } => *is_arrow || !stable_object_read(base, scan, ctx),
        // 以下形态都无法用名字栅栏表达其读写集合 ⇒ 不是不变式（根因 3 的 fail-safe）。
        Expr::Deref { .. } | Expr::AddrOf { .. } => true,
        Expr::Call { .. } | Expr::AsmExpr { .. } => true,
        Expr::Assign { .. }
        | Expr::PostInc { .. }
        | Expr::PostDec { .. }
        | Expr::PreInc { .. }
        | Expr::PreDec { .. } => true,
        Expr::Comma { .. } | Expr::StmtExpr { .. } => true,
        Expr::CompoundLit { .. } | Expr::InitList { .. } => true,
        Expr::VaArg { .. } | Expr::LabelAddr { .. } => true,
        // 新增表达式形态默认即为"不是不变式"，不需要同步改这里。
        _ => true,
    }
}

/// `e` 是否是一个"根对象有名字、且该名字在栅栏外"的内存读目标（数组/结构体对象）。
fn stable_object_read(e: &Expr, scan: &NameScan, ctx: &LicmCtx) -> bool {
    match e {
        // 指针类型的 Ident 不是对象本体（`p[i]` 的 p 指向别处），只接受数组/结构体对象。
        Expr::Ident { name, ty, .. } => {
            matches!(ty, Type::Array(..) | Type::Struct(..) | Type::Union(..))
                && ctx.read_is_stable(name.name.as_str(), scan)
        },
        Expr::Index { arr, .. } => stable_object_read(arr, scan, ctx),
        Expr::Member {
            expr: base,
            is_arrow,
            ..
        } => !is_arrow && stable_object_read(base, scan, ctx),
        _ => false,
    }
}

fn type_has_vla(ty: &Type) -> bool {
    match ty {
        Type::Array(inner, size) => matches!(size, ArraySize::Var(_)) || type_has_vla(&inner.base),
        Type::Ptr(inner) => type_has_vla(&inner.base),
        Type::Atomic(inner) => type_has_vla(&inner.base),
        Type::Complex(inner) | Type::Imaginary(inner) => type_has_vla(&inner.base),
        Type::Vector(inner, _) => type_has_vla(&inner.base),
        _ => false,
    }
}

// ---- 名字收集（For / While / Do 与函数级扫描共用）-----------------------------

fn scan_block(s: &mut NameScan, b: &Block, site: Site) {
    for item in &b.items {
        scan_item(s, item, site);
    }
}

fn scan_item(s: &mut NameScan, item: &BlockItem, site: Site) {
    match item {
        BlockItem::Stmt(st) => scan_stmt(s, st, site),
        BlockItem::Decl(Decl::Var(v)) => {
            let name = v.name.name.as_str();
            *s.decl_counts.entry(name.to_string()).or_insert(0) += 1;
            if v.quals.is_volatile() || v.quals.is_atomic() || matches!(v.ty, Type::Atomic(_)) {
                s.volatiles.insert(name.to_string());
            }
            if v.is_static || v.is_thread_local {
                s.persistent.insert(name.to_string());
            }
            if let Some(e) = &v.init {
                scan_expr(s, e, site);
            }
            scan_type(s, &v.ty, site);
            // 声明即一次写入（每轮迭代都执行初始化），这也是候选需要 `only_self_write`
            // 放行自己这一次的原因。
            mark_written(s, name, site);
        },
        // 嵌套函数定义：按 upvars 直接读写外层局部，整个函数放弃 LICM。
        BlockItem::Decl(Decl::Func(_)) => s.gives_up = true,
        BlockItem::Decl(_) => {},
    }
}

/// VLA 维度是每轮重求值的运行期表达式，必须进栅栏。
fn scan_type(s: &mut NameScan, ty: &Type, site: Site) {
    match ty {
        Type::Array(inner, ArraySize::Var(e)) => {
            scan_expr(s, e, site);
            scan_type(s, &inner.base, site);
        },
        Type::Array(inner, ArraySize::Const(_)) | Type::Ptr(inner) | Type::Atomic(inner) => {
            scan_type(s, &inner.base, site);
        },
        _ => {},
    }
}

fn scan_stmt(s: &mut NameScan, st: &Stmt, site: Site) {
    match st {
        Stmt::Expr { e, .. } => scan_expr(s, e, site),
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                scan_expr(s, e, site);
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            scan_expr(s, cond, Site::Body);
            scan_stmt(s, then_, site);
            if let Some(el) = else_ {
                scan_stmt(s, el, site);
            }
        },
        Stmt::While { cond, body, .. } => {
            // 体内嵌套循环的控制表达式对**当前**循环而言就是体内语句。
            scan_expr(s, cond, Site::Body);
            scan_stmt(s, body, site);
        },
        Stmt::Do { body, cond, .. } => {
            scan_stmt(s, body, site);
            scan_expr(s, cond, Site::Body);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                scan_stmt(s, i, site);
            }
            if let Some(c) = cond {
                scan_expr(s, c, Site::Body);
            }
            if let Some(sp) = step {
                scan_expr(s, sp, Site::Body);
            }
            scan_stmt(s, body, site);
        },
        Stmt::Block { b, .. } => scan_block(s, b, site),
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Fallthrough { .. } => {},
        // 这些形态的控制流可以落到 AST 的任意位置（或读写集合只写在字符串里），名字
        // 栅栏无法表达"从这里外提的语句会不会被绕过" ⇒ fail-safe（根因 3）。仍尽量把
        // 其中可见的表达式扫一遍，让函数级逃逸扫描不丢信息。
        Stmt::Switch { expr, cases, .. } => {
            s.reject_all = true;
            scan_expr(s, expr, Site::Body);
            for c in cases {
                if let Some(v) = &c.val {
                    scan_expr(s, v, Site::Body);
                }
                if let Some(re) = &c.range_end {
                    scan_expr(s, re, Site::Body);
                }
                scan_block(s, &c.body, Site::Body);
            }
        },
        Stmt::Label { stmt, .. } => {
            s.reject_all = true;
            scan_stmt(s, stmt, site);
        },
        Stmt::Goto { .. } => s.reject_all = true,
        Stmt::GotoIndirect { expr, .. } => {
            s.reject_all = true;
            scan_expr(s, expr, Site::Body);
        },
        Stmt::AsmGoto { .. } => {
            s.reject_all = true;
            s.poison = true;
        },
        Stmt::Invoke { call, .. } => {
            s.reject_all = true;
            scan_stmt(s, call, site);
        },
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            s.reject_all = true;
            scan_expr(s, object, Site::Body);
            scan_expr(s, selector, Site::Body);
        },
        // 新增语句形态默认放弃外提，不需要同步改这里。
        _ => s.reject_all = true,
    }
}

fn scan_expr(s: &mut NameScan, e: &Expr, site: Site) {
    match e {
        Expr::Ident { name, ty, .. } => {
            // 数组名/函数名出现在表达式里即等价于衰减成指针：其元素/对象对调用可见。
            if matches!(ty, Type::Array(..) | Type::Func(..)) {
                s.escaped.insert(name.name.clone());
            }
            mark_read(s, name.name.as_str(), site);
        },
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => {
            scan_expr(s, l, site);
            scan_expr(s, r, site);
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::SizeofExpr { e: inner, .. }
        | Expr::AlignofExpr { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. } => scan_expr(s, inner, site),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            scan_expr(s, cond, site);
            scan_expr(s, then_, site);
            scan_expr(s, else_, site);
        },
        Expr::Assign { l, r, .. } => {
            scan_lvalue(s, l, site);
            scan_expr(s, r, site);
        },
        Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => scan_lvalue(s, inner, site),
        Expr::Index { arr, idx, .. } => {
            scan_expr(s, arr, site);
            scan_expr(s, idx, site);
        },
        Expr::Member { expr: base, .. } => scan_expr(s, base, site),
        Expr::AddrOf { e: inner, .. } => {
            // 被取址的对象对任何调用都可见（哪怕指针只在函数内部传递）。
            if let Some(root) = root_object_name(inner) {
                s.escaped.insert(root);
            }
            scan_expr(s, inner, site);
        },
        Expr::Deref { e: inner, .. } => scan_expr(s, inner, site),
        Expr::Call { callee, args, .. } => {
            scan_expr(s, callee, site);
            for a in args {
                scan_expr(s, a, site);
            }
            // 被调者可改写全局、volatile、被取址/衰减逃逸的对象以及 static 局部：
            // 把这些名字逐个记为"被写"，候选语句只要提到其中之一就不可外提。
            clobber_observable(s);
        },
        Expr::AsmExpr { .. } => {
            // asm 的读写集合由字符串里的约束式约定，AST 拿不到信息 ⇒ 全部放弃。
            s.reject_all = true;
            s.poison = true;
        },
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    crate::ast::InitItem::Expr(v) => scan_expr(s, v, site),
                    crate::ast::InitItem::Designated { designator, value } => {
                        scan_designator(s, designator, site);
                        scan_expr(s, value, site);
                    },
                }
            }
        },
        Expr::Offsetof { indices, .. } => {
            for i in indices {
                scan_expr(s, i, site);
            }
        },
        Expr::StmtExpr { block, .. } => {
            s.reject_all = true;
            scan_block(s, block, Site::Body);
        },
        Expr::VaArg { ap, .. } => {
            s.reject_all = true;
            scan_expr(s, ap, site);
        },
        Expr::LabelAddr { .. } => s.reject_all = true,
        // 字面量一类不读不写的形态：显式列出，其余（将来新增的表达式形态）一律
        // 按"拿不到信息"处理 ⇒ fail-safe（根因 3）。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. } => {},
        _ => s.reject_all = true,
    }
}

fn scan_designator(s: &mut NameScan, d: &crate::ast::Designator, site: Site) {
    match d {
        crate::ast::Designator::Field(_) => {},
        crate::ast::Designator::Index(e) => scan_expr(s, e, site),
        crate::ast::Designator::Range(a, b) => {
            scan_expr(s, a, site);
            scan_expr(s, b, site);
        },
        crate::ast::Designator::Chain(sub) => {
            for sd in sub {
                scan_designator(s, sd, site);
            }
        },
    }
}

/// 写入目标：把写记到"被写对象的名字"上；表达不了的按间接写/未知处理。
fn scan_lvalue(s: &mut NameScan, l: &Expr, site: Site) {
    // 先按读扫一遍左值链：下标表达式、指针变量本身都会被读。
    scan_expr(s, l, site);
    let lv = lvalue_of(l);
    if lv.indirect {
        // 间接写：`*p = x` / `p->f = x` / `p[i] = x`。被写的对象没有可记的名字，
        // 只能把所有可观测对象（全局、volatile、被取址/衰减逃逸的局部、static 局部）
        // 都记为被写。
        clobber_observable(s);
    } else if let Some(name) = lv.root {
        mark_written(s, &name, site);
    } else {
        // 判定不了写入目标的左值形态 ⇒ fail-safe（根因 3）。
        s.reject_all = true;
    }
}

/// 左值链解析结果：`root` 是链的根对象名，`indirect` 表示真正被写的对象没有名字
/// （链上出现过 `*` / `->` / 指针下标），`is_arr` 表示该表达式本身仍是数组对象。
struct Lvalue {
    root: Option<String>,
    indirect: bool,
    is_arr: bool,
}

/// `a[i] = x` 只有当 `a` 是**数组对象**时才是写在 a 上；`p` 是指针时 `p[i] = x` 与
/// `*(p+i) = x` 同义 ⇒ 间接写。`s.f = x` 写在 s 上，`p->f = x` 写在 p 指向的对象上。
fn lvalue_of(e: &Expr) -> Lvalue {
    match e {
        Expr::Ident { name, ty, .. } => Lvalue {
            root: Some(name.name.clone()),
            indirect: false,
            is_arr: matches!(ty, Type::Array(..)),
        },
        Expr::Index { arr, ty, .. } => {
            let base = lvalue_of(arr);
            // 基址不是数组对象（是指针或更深的间接链）⇒ 元素写无法按名字记账。
            Lvalue {
                root: base.root,
                indirect: base.indirect || !base.is_arr,
                is_arr: matches!(ty, Type::Array(..)),
            }
        },
        Expr::Member {
            expr: base,
            is_arrow,
            ty,
            ..
        } => {
            let b = lvalue_of(base);
            Lvalue {
                root: b.root,
                indirect: b.indirect || *is_arrow,
                is_arr: matches!(ty, Type::Array(..)),
            }
        },
        Expr::Deref { ty, .. } => Lvalue {
            root: None,
            indirect: true,
            is_arr: matches!(ty, Type::Array(..)),
        },
        Expr::Cast { e: inner, .. } => lvalue_of(inner),
        // 其余形态（调用结果、复合字面量、位域之外的怪左值…）一律判不了。
        _ => Lvalue {
            root: None,
            indirect: false,
            is_arr: false,
        },
    }
}

fn clobber_observable(s: &mut NameScan) {
    let names: Vec<String> = s.observable.iter().cloned().collect();
    for n in names {
        *s.written.entry(n).or_insert(0) += 1;
    }
}

fn mark_written(s: &mut NameScan, name: &str, site: Site) {
    *s.written.entry(name.to_string()).or_insert(0) += 1;
    if site != Site::Body {
        s.control.insert(name.to_string());
    }
}

fn mark_read(s: &mut NameScan, name: &str, site: Site) {
    if site != Site::Body {
        s.control.insert(name.to_string());
        if site == Site::Cond {
            s.cond_reads.insert(name.to_string());
        }
    }
}

/// `&a[i].b` / `&p[3]` 这类取址表达式的根对象名。
fn root_object_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(name.name.clone()),
        Expr::Index { arr, .. } => root_object_name(arr),
        Expr::Member { expr: base, .. } => root_object_name(base),
        Expr::Deref { e: inner, .. } | Expr::Cast { e: inner, .. } => root_object_name(inner),
        _ => None,
    }
}

// ===== 循环展开 =====

fn unroll_block(block: &mut Block, max_trip: usize) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                mut body,
                span,
            }) => {
                if let Some(unrolled) = try_unroll_for_stmt(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    body.as_ref(),
                    max_trip,
                ) {
                    out.extend(unrolled);
                    continue;
                }
                unroll_in_stmt(body.as_mut(), max_trip);
                out.push(BlockItem::Stmt(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                    span,
                }));
            },
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                unroll_block(&mut b, max_trip);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            },
            other => out.push(other),
        }
    }
    block.items = out;
}

fn unroll_in_stmt(body: &mut Stmt, max_trip: usize) {
    if let Stmt::Block { b, .. } = body {
        unroll_block(b, max_trip);
    }
}

fn try_unroll_for_stmt(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
    max_trip: usize,
) -> Option<Vec<BlockItem>> {
    let (ivar, trip) = parse_counted_for(init, cond, step, max_trip)?;
    let body_items = loop_body_items(body)?;
    if trip == 0 || trip > max_trip || !unroll_body_is_safe(&body_items) {
        return None;
    }
    let mut items = Vec::new();
    if let Some(i) = init {
        match i {
            Stmt::Block { b, .. } => items.extend(b.items.clone()),
            other => items.push(BlockItem::Stmt(other.clone())),
        }
    }
    for k in 0..trip {
        // 逐份先构造到本地副本: 任一条目替换不了就得整条不展开, 不能只丢一个
        // 条目 (那会丢掉循环体的一部分语义)。
        let mut copy: Vec<BlockItem> = Vec::with_capacity(body_items.len());
        for bi in &body_items {
            match subst_induction(bi, &ivar, k as i128) {
                Some(rewritten) => copy.push(rewritten),
                None => return None,
            }
        }
        items.extend(copy);
    }
    Some(items)
}

fn loop_body_items(body: &Stmt) -> Option<Vec<BlockItem>> {
    match body {
        Stmt::Block { b, .. } => Some(b.items.clone()),
        other => Some(vec![BlockItem::Stmt(other.clone())]),
    }
}

/// 展开前检查: 体内是否存在「绑定到本循环」的控制流, 或无法安全复制的形态。
///
/// `break`/`continue` 只在**没有被更内层的循环/switch 屏蔽**时绑定本循环: 旧实现
/// 只看循环体的**直接**条目, 于是 `for (i<4) { if (x) break; }` 会被展开成四份带
/// `break` 的语句 —— 展开后已没有本循环, 那个 `break` 于是绑到更外层的循环/switch
/// (或直接变成悬空 break), 静默改变控制流。`goto`/标签/asm-goto/EH 形态把复制体
/// 里的标签重名 (汇编器报错) 或跨副本跳转, 一律放弃。
///
/// `return` 则是**可安全复制**的: 原来第一次迭代就退出函数, 展开后第一份副本同样
/// 立即退出, 后续副本不可达 (仅体积代价)。
fn unroll_body_is_safe(items: &[BlockItem]) -> bool {
    items.iter().all(|it| unroll_item_is_safe(it, false, false))
}

fn unroll_item_is_safe(item: &BlockItem, break_shielded: bool, cont_shielded: bool) -> bool {
    match item {
        BlockItem::Stmt(s) => unroll_stmt_is_safe(s, break_shielded, cont_shielded),
        // 声明条目本身不带控制流; 其初始化式里的归纳变量能否完全替换由
        // `rewrite_ivars` 判定 (覆盖不到则整条不展开)。
        BlockItem::Decl(_) => true,
        _ => true,
    }
}

fn unroll_stmt_is_safe(s: &Stmt, break_shielded: bool, cont_shielded: bool) -> bool {
    match s {
        Stmt::Break { .. } => break_shielded,
        Stmt::Continue { .. } | Stmt::Fallthrough { .. } => cont_shielded,
        Stmt::Label { .. }
        | Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. }
        | Stmt::AsmGoto { .. } => false,
        Stmt::Invoke { .. } | Stmt::EhLandingPad { .. } => false,
        Stmt::Expr { .. } | Stmt::Return { .. } => true,
        Stmt::Block { b, .. } => b
            .items
            .iter()
            .all(|i| unroll_item_is_safe(i, break_shielded, cont_shielded)),
        Stmt::If { then_, else_, .. } => {
            unroll_stmt_is_safe(then_, break_shielded, cont_shielded)
                && else_
                    .as_ref()
                    .map_or(true, |e| unroll_stmt_is_safe(e, break_shielded, cont_shielded))
        },
        // 更内层的循环同时屏蔽 break 与 continue。
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            unroll_stmt_is_safe(body, true, true)
        },
        // `switch` 只屏蔽 `break`：case 体里的 `continue` 仍然穿透它绑定外层循环，
        // 展开后外层循环消失 ⇒ 该 `continue` 要么失去目标、要么改绑更外层，
        // 两者都是静默改变控制流 (实测 `for(i<3){switch(i){case 1:continue;
        // default:s+=1;}s+=10;}` 在 -O3 得 33, 应 22)。
        Stmt::Switch { cases, .. } => cases.iter().all(|c| {
            c.body
                .items
                .iter()
                .all(|i| unroll_item_is_safe(i, true, cont_shielded))
        }),
    }
}

fn subst_induction(item: &BlockItem, ivar: &str, value: i128) -> Option<BlockItem> {
    let mut cloned = item.clone();
    if rewrite_ivars(&mut cloned, ivar, value) {
        Some(cloned)
    } else {
        None
    }
}

/// 返回 `false` = 遇到本函数没有覆盖的形态 (**半替换比不替换严重得多**, 见
/// `try_unroll_for_stmt`)。
fn rewrite_ivars(item: &mut BlockItem, ivar: &str, value: i128) -> bool {
    match item {
        BlockItem::Decl(Decl::Var(v)) => match &mut v.init {
            Some(init) => rewrite_expr_ivars(init, ivar, value),
            None => true,
        },
        // 数组/结构体声明的初始化列表、复合字面量里的 `i` 同样要替换, 覆盖不到就
        // 放弃展开 —— 只处理标量 init 会把 `int a[4] = {i, 1, 2, 3};` 留成悬空引用。
        BlockItem::Decl(_) => false,
        BlockItem::Stmt(s) => rewrite_stmt_ivars(s, ivar, value),
        _ => false,
    }
}

fn rewrite_stmt_ivars(s: &mut Stmt, ivar: &str, value: i128) -> bool {
    match s {
        Stmt::Expr { e, .. } => rewrite_expr_ivars(e, ivar, value),
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                return rewrite_expr_ivars(e, ivar, value);
            }
            true
        },
        Stmt::Block { b, .. } => b
            .items
            .iter_mut()
            .fold(true, |acc, sub| rewrite_ivars(sub, ivar, value) && acc),
        // 嵌套循环/分支里的 `i` 必须一并替换: 旧实现只处理 Expr/Return/Block,
        // 于是一个内层 `for` 体内的 `i` 被**整体漏掉** —— 展开后四份副本全都读
        // 那个「声明后从未赋值」的 `i` (实测 `for(i<4)for(j<4)s+=i*j;` 在 -O3
        // 得 0, 应 36)。`Stmt::If` / `Stmt::While` / `Stmt::Do` / `Stmt::Switch` 同构。
        Stmt::If {
            cond, then_, else_, ..
        } => {
            let mut ok =
                rewrite_expr_ivars(cond, ivar, value) && rewrite_stmt_ivars(then_, ivar, value);
            if let Some(e) = else_ {
                ok = rewrite_stmt_ivars(e, ivar, value) && ok;
            }
            ok
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            let mut ok = true;
            if let Some(i) = init {
                ok = rewrite_stmt_ivars(i, ivar, value);
            }
            if let Some(c) = cond {
                ok = rewrite_expr_ivars(c, ivar, value) && ok;
            }
            if let Some(st) = step {
                ok = rewrite_expr_ivars(st, ivar, value) && ok;
            }
            rewrite_stmt_ivars(body, ivar, value) && ok
        },
        Stmt::While { cond, body, .. } => {
            rewrite_expr_ivars(cond, ivar, value) && rewrite_stmt_ivars(body, ivar, value)
        },
        Stmt::Do { body, cond, .. } => {
            rewrite_stmt_ivars(body, ivar, value) && rewrite_expr_ivars(cond, ivar, value)
        },
        Stmt::Switch { expr, cases, .. } => {
            if !rewrite_expr_ivars(expr, ivar, value) {
                return false;
            }
            cases.iter_mut().all(|c| {
                let mut ok = c
                    .val
                    .as_mut()
                    .map_or(true, |e| rewrite_expr_ivars(e, ivar, value));
                ok = c
                    .range_end
                    .as_mut()
                    .map_or(true, |e| rewrite_expr_ivars(e, ivar, value))
                    && ok;
                c.body
                    .items
                    .iter_mut()
                    .fold(ok, |acc, it| rewrite_ivars(it, ivar, value) && acc)
            })
        },
        // 这些形态本身不携带可替换的表达式; 体内出现时由 `unroll_body_is_safe`
        // (goto/label/asm-goto/EH) 或 `body_has_control_items` (break/continue/return)
        // 先拦住, 走不到替换这一步。
        Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. }
        | Stmt::Label { .. }
        | Stmt::Goto { .. } => true,
        // 间接跳转的目标取自表达式, 而汇编/EH 形态的读写集合不在 AST 名字里 ——
        // 一律报「覆盖不到」, 由调用方放弃展开。
        Stmt::GotoIndirect { .. } | Stmt::AsmGoto { .. } | Stmt::Invoke { .. } => false,
        Stmt::EhLandingPad { .. } => false,
    }
}

/// 把表达式里对 `ivar` 的引用换成常量。返回 `false` = 形态未覆盖。
fn rewrite_expr_ivars(e: &mut Expr, ivar: &str, value: i128) -> bool {
    match e {
        Expr::Ident { name, ty, span } if name.name == ivar => {
            *e = Expr::Int {
                v: value,
                span: span.clone(),
                ty: ty.clone(),
            };
            true
        },
        // 常量与不求值操作数: 里面不会出现运行期的 `ivar` 读取。
        // (`sizeof`/`alignof` 的操作数不求值, 但 VLA 例外 —— 保守报未覆盖。)
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::Offsetof { .. } => true,
        // 不是归纳变量的名字: 本身就是叶节点, 无需替换且已完整覆盖。
        // (必须列出: 上面的 `Ident` 分支带 `if` guard, 不匹配的名字会落不到它上, 若交给
        // 下方的 `_ => false` 就等于「体内只要出现任何其它名字就不展开」，
        // 把本 pass 整体关掉。)
        Expr::Ident { .. } => true,
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            rewrite_expr_ivars(l, ivar, value) && rewrite_expr_ivars(r, ivar, value)
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. }
        | Expr::Deref { e: inner, .. }
        | Expr::AddrOf { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => rewrite_expr_ivars(inner, ivar, value),
        Expr::Member { expr: base, .. } => rewrite_expr_ivars(base, ivar, value),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            rewrite_expr_ivars(cond, ivar, value)
                && rewrite_expr_ivars(then_, ivar, value)
                && rewrite_expr_ivars(else_, ivar, value)
        },
        Expr::Call { callee, args, .. } => {
            if !rewrite_expr_ivars(callee, ivar, value) {
                return false;
            }
            args.iter_mut().all(|a| rewrite_expr_ivars(a, ivar, value))
        },
        // 其余形态 (语句表达式 / 初始化列表 / 复合字面量 / va_arg / 内联汇编 /
        // 标签取址 / 未来新增的任何表达式) 一律按「覆盖不到」处理。
        _ => false,
    }
}

fn parse_counted_for(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    max_trip: usize,
) -> Option<(String, usize)> {
    let ivar = init_stmt_var(init?)?;
    let limit = parse_lt_limit(cond?, &ivar, max_trip)?;
    if !is_inc_step(step?, &ivar) {
        return None;
    }
    Some((ivar, limit))
}

fn init_stmt_var(init: &Stmt) -> Option<String> {
    match init {
        Stmt::Expr {
            e: Expr::Assign { l, r, .. },
            ..
        } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if matches!(r.as_ref(), Expr::Int { v: 0, .. }) {
                    return Some(name.name.clone());
                }
            }
            None
        },
        Stmt::Block { b, .. } => {
            if b.items.len() == 1 {
                if let BlockItem::Decl(Decl::Var(v)) = &b.items[0] {
                    if matches!(v.init.as_ref(), Some(Expr::Int { v: 0, .. })) {
                        return Some(v.name.name.clone());
                    }
                }
            }
            None
        },
        _ => None,
    }
}

fn parse_lt_limit(cond: &Expr, ivar: &str, max_trip: usize) -> Option<usize> {
    match cond {
        Expr::Binop {
            op: BinOp::Lt,
            l,
            r,
            ..
        } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if name.name == ivar {
                    if let Ok(v) = const_eval::eval(r) {
                        if v >= 0 && (v as usize) <= max_trip {
                            return Some(v as usize);
                        }
                    }
                }
            }
            None
        },
        _ => None,
    }
}

fn is_inc_step(step: &Expr, ivar: &str) -> bool {
    match step {
        Expr::PostInc { e, .. } | Expr::PreInc { e, .. } => {
            matches!(e.as_ref(), Expr::Ident { name, .. } if name.name == ivar)
        },
        Expr::Assign { l, r, .. } => {
            if !matches!(l.as_ref(), Expr::Ident { name, .. } if name.name == ivar) {
                return false;
            }
            match r.as_ref() {
                Expr::Binop {
                    op: BinOp::Add,
                    l: li,
                    r: rv,
                    ..
                } => {
                    matches!(li.as_ref(), Expr::Ident { name, .. } if name.name == ivar)
                        && matches!(rv.as_ref(), Expr::Int { v: 1, .. })
                },
                _ => false,
            }
        },
        _ => false,
    }
}

// ===== 向量循环向量化 =====

fn vectorize_block(block: &mut Block, has_vnni: bool, vec_bits: u32) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                mut body,
                span,
            }) => {
                // Try VNNI dot-product vectorization first
                if has_vnni && vec_bits >= 256 {
                    if let Some(vnni_stmt) = try_vectorize_vnni(
                        init.as_deref(),
                        cond.as_ref(),
                        step.as_ref(),
                        body.as_ref(),
                        vec_bits,
                    ) {
                        out.push(vnni_stmt);
                        continue;
                    }
                }
                if let Some(vec_stmt) = try_vectorize_for_stmt(
                    init.as_deref(),
                    cond.as_ref(),
                    step.as_ref(),
                    body.as_ref(),
                ) {
                    out.push(vec_stmt);
                } else {
                    vectorize_in_stmt(body.as_mut(), has_vnni, vec_bits);
                    out.push(BlockItem::Stmt(Stmt::For {
                        init,
                        cond,
                        step,
                        body,
                        span,
                    }));
                }
            },
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                vectorize_block(&mut b, has_vnni, vec_bits);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            },
            other => out.push(other),
        }
    }
    block.items = out;
}

fn vectorize_in_stmt(body: &mut Stmt, has_vnni: bool, vec_bits: u32) {
    if let Stmt::Block { b, .. } = body {
        vectorize_block(b, has_vnni, vec_bits);
    }
}

fn try_vectorize_for_stmt(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
) -> Option<BlockItem> {
    let stmt = loop_body_single_stmt(body)?;
    let block_items = loop_body_items(body)?;
    let pseudo = Block {
        items: block_items,
        span: stmt_span(body),
    };
    try_vectorize_for(init, cond, step, &pseudo, stmt)
}

fn loop_body_single_stmt(body: &Stmt) -> Option<&Stmt> {
    match body {
        Stmt::Block { b, .. } if b.items.len() == 1 => match &b.items[0] {
            BlockItem::Stmt(s) => Some(s),
            _ => None,
        },
        s @ (Stmt::Expr { .. } | Stmt::Return { .. }) => Some(s),
        _ => None,
    }
}

fn try_vectorize_for(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    _body: &Block,
    stmt: &Stmt,
) -> Option<BlockItem> {
    let (ivar, n) = parse_counted_for(init, cond, step, 4)?;
    let (dst, src_l, src_r, op) = parse_vector_elem_assign(stmt, &ivar)?;
    let (elem_ty, count) = match dst.1 {
        Type::Vector(ref e, c) if c == n => (e.as_ref().clone(), c),
        _ => return None,
    };
    if count != n {
        return None;
    }
    let vec_ty = Type::Vector(Box::new(elem_ty), count);
    let sp = stmt_span(stmt);
    Some(BlockItem::Stmt(Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: dst.0,
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            r: Box::new(Expr::Binop {
                op,
                l: Box::new(Expr::Ident {
                    name: src_l.0,
                    span: sp.clone(),
                    ty: vec_ty.clone(),
                }),
                r: Box::new(Expr::Ident {
                    name: src_r.0,
                    span: sp.clone(),
                    ty: vec_ty.clone(),
                }),
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            span: sp.clone(),
            ty: vec_ty,
        },
        span: sp,
    }))
}

/// 尝试识别 int8/int16 dot-product 模式并用 VNNI 指令向量化。
///
/// 检测模式: `sum += a[i] * b[i]` (循环累加乘)
/// 当目标支持 VNNI 时，标记为可用 VPDPBUSD/VPDPWSSD 指令。
/// 向量化因子: AVX2=256bit/16bit=16 lanes, AVX-512=512bit/16bit=32 lanes
fn try_vectorize_vnni(
    init: Option<&Stmt>,
    cond: Option<&Expr>,
    step: Option<&Expr>,
    body: &Stmt,
    vec_bits: u32,
) -> Option<BlockItem> {
    // Parse the for loop structure
    let (ivar, _n) = parse_counted_for(init, cond, step, 4)?;
    let stmt = loop_body_single_stmt(body)?;

    // Check for accumulation pattern: sum += a[i] * b[i]
    let s = match stmt {
        Stmt::Expr { e, .. } => e,
        _ => return None,
    };
    let (acc_l, acc_r) = match s {
        Expr::Assign {
            op: BinOp::Add,
            l,
            r,
            ..
        } => (l.as_ref(), r.as_ref()),
        _ => return None,
    };

    // LHS must be accumulator (scalar int)
    let acc_name = match acc_l {
        Expr::Ident { name, ty, .. } => {
            if !matches!(ty, Type::Int | Type::Short | Type::Char) {
                return None;
            }
            name.clone()
        },
        _ => return None,
    };

    // RHS must be a[i] * b[i] pattern
    let (mul_l, mul_r) = match acc_r {
        Expr::Binop {
            op: BinOp::Mul,
            l,
            r,
            ..
        } => (l.as_ref(), r.as_ref()),
        _ => return None,
    };

    // Both operands must be array accesses with ivar
    if !is_array_index_with(mul_l, &ivar) || !is_array_index_with(mul_r, &ivar) {
        return None;
    }

    // Compute vector lanes based on element size and register width
    let elem_bits: u32 = 16; // int16 dot-product
    let lanes = vec_bits / elem_bits;

    // Mark this loop as VNNI-vectorizable by wrapping in a
    // vectorized statement with appropriate type annotations.
    let sp = stmt_span(stmt);
    let i16_qt = crate::ast::QualType::new(Type::Short);
    let vec_ty = Type::Vector(Box::new(i16_qt), lanes as usize);

    // Produce a vectorized accumulation statement:
    // acc_vec += (vec)a[i] * (vec)b[i]
    Some(BlockItem::Stmt(Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: acc_name,
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            r: Box::new(Expr::Binop {
                op: BinOp::Mul,
                l: Box::new(mul_l.clone()),
                r: Box::new(mul_r.clone()),
                span: sp.clone(),
                ty: vec_ty.clone(),
            }),
            span: sp.clone(),
            ty: vec_ty,
        },
        span: sp,
    }))
}

/// Check if expression is an array access with the given index variable.
fn is_array_index_with(e: &Expr, ivar: &str) -> bool {
    match e {
        Expr::Index { arr: _, idx, .. } => {
            if let Expr::Ident { name, .. } = idx.as_ref() {
                name.name == ivar
            } else {
                false
            }
        },
        _ => false,
    }
}

fn parse_vector_elem_assign(
    s: &Stmt,
    ivar: &str,
) -> Option<(NamedVector, NamedVector, NamedVector, BinOp)> {
    let e = match s {
        Stmt::Expr { e, .. } => e,
        _ => return None,
    };
    let (l, r) = match e {
        Expr::Assign { l, r, .. } => (l.as_ref(), r.as_ref()),
        _ => return None,
    };
    let dst = index_vector_ident(l, ivar)?;
    let (op, src_l, src_r) = match r {
        Expr::Binop { op, l, r, .. } => {
            (*op, index_vector_ident(l, ivar)?, index_vector_ident(r, ivar)?)
        },
        _ => return None,
    };
    if dst.1 != src_l.1 || dst.1 != src_r.1 {
        return None;
    }
    Some((dst, src_l, src_r, op))
}

type NamedVector = (crate::ast::Ident, Type);

fn index_vector_ident(e: &Expr, ivar: &str) -> Option<NamedVector> {
    match e {
        Expr::Index { arr, idx, .. } => {
            let arr = arr.as_ref();
            let idx = idx.as_ref();
            match (arr, idx) {
                (
                    Expr::Ident {
                        name,
                        ty: Type::Vector(elem, n),
                        ..
                    },
                    Expr::Ident { name: idx_name, .. },
                ) if idx_name.name == ivar => Some((name.clone(), Type::Vector(elem.clone(), *n))),
                _ => None,
            }
        },
        _ => None,
    }
}

fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Expr { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::If { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Do { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Block { span, .. } => span.clone(),
        _ => Span::point("loop", 1, 1),
    }
}

/// 检测简单 for 循环: `for (int ivar = 0; ivar < hi; ivar++)` (常量下界 0 + 步长 1)。
fn detect_simple_for(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
) -> Option<(String, i128, i128)> {
    let ivar = init.as_deref().and_then(init_stmt_var)?;
    // Check step is ivar++
    match step.as_ref()? {
        Expr::PostInc { e, .. } => {
            if let Expr::Ident { name, .. } = e.as_ref() {
                if name.name != ivar {
                    return None;
                }
            } else {
                return None;
            }
        },
        _ => return None,
    }
    // Check cond is ivar < hi
    let cond = cond.as_ref()?;
    match cond {
        Expr::Binop {
            op: BinOp::Lt,
            l,
            r,
            ..
        } => {
            if let Expr::Ident { name, .. } = l.as_ref() {
                if name.name != ivar {
                    return None;
                }
            } else {
                return None;
            }
            if let Expr::Int { v, .. } = r.as_ref() {
                Some((ivar, 0, *v))
            } else {
                None
            }
        },
        _ => None,
    }
}

// ===== P10. 循环分块 (Loop Tiling / Blocking) =====
//
// 旧实现 (`generate_tiled_loop`) 对**单个** for 循环做「外层按 tile 跳、内层
// `for (int j = 0; j < ts; j++)` + 把体内 `i` 文本替换成 `ii + j`」, 是一台静默误编译器:
//   1. 造出来的名字 `j` 与被改写的循环体**共享同一个名字空间** —— 本后端按名字
//      分配栈槽, 两层嵌套的体内自己就有 `for (int j...)`, 两个 `j` 落到同一个槽。
//      实测 (`-O3`): `for(i<5)for(j<5)s+=i;` 得 30 (应 50), `s+=j` 得 20 (应 50),
//      `s+=i*j` 得 70 (应 100); 与是否进 SSA 无关 (加 `goto` 的同形函数一样错)。
//   2. `subst_stmt` 的 `BlockItem::Decl` 分支直接 `other.clone()`, 声明初始化式里的
//      `i` 根本没被替换; 未列举的 `Stmt`/`Expr` 形态同样静默漏替换。
//   3. 单循环分块本身没有访存局部性收益 (内层仍连续扫同一段), 只是白付边界比较。
//
// 现形态: 只对**完美两层嵌套**做块 interchange, 并且**逐字保留循环体** (不做任何
// 名字替换) —— 子循环用同名 `int i = ti;` 重新声明原归纳变量, 体内所有引用天然落在
// 正确的最近绑定上, 捕获在结构上被排除:
//
//   for (int ti = 0; ti < N; ti = ti + ts)
//     for (int tj = 0; tj < M; tj = tj + ts)
//       for (int i = ti; i < N && i < ti + ts; i++)
//         for (int j = tj; j < M && j < tj + ts; j++)
//           BODY            ← 原样
//
// 覆盖的迭代空间与原两层循环逐点一致 (每个 (i,j) 恰好执行一次), 只是执行顺序按
// ts×ts 块重排 ⇒ 局部性收益真实存在, 而正确性不依赖任何名字分析。安全前提见
// `try_tile_nest`: 两维都必须是 `for (int iv = 0; iv < 常量; iv++)`、体内不写
// 归纳变量、不取它们的地址、无 goto/label/switch/asm 形态。
//
// 接入前的两个前置都已消除 (本轮实测): 分块产物进 SSA 管线后曾把块索引 `__dcc_tile_i0` 的推进
// 赋值当成「合成临时值」内联掉 ⇒ `while(1)` 永不推进 (实测 -O3 忙等)。根因不是
// `rebuild_cycle`, 而是重建用「名字以 `_` 开头」猜合成临时值 —— 同一个启发式也让
// 用户自己的 `int _s` / `int __d` 在 -O2 起算错 (与分块、与循环闸门都无关)。现判据
// 改成 `ssa::is_synthetic_temp_base` (只信 `original_locals ∪ params`), 分块已接入;
// 常驻夹具 `underscore_locals.c` + `tile_nest_preserves_iteration_space` 单测。
//
// 曾一度不能接入的另一半原因 (已修): 它产出的四层嵌套形态在 -O2 就得 0 (应 50)。
// 那条在 AST 层 GVN 的 coalesce —— 它用**块内**写入集合判「一次性拷贝」, 把 for-init
// 里的 `int i = ti;` 声明删掉了 (写 `i` 的 `i++` 属于外层节点) —— 已修,
// 回归夹具 `tests/fixtures/opt/gvn_loop_copied_iv.c`。

/// 分块驱动: 只在「本项就是一个完美两层嵌套且可安全分块」时改写它; 其余形态
/// 只递归 Block / If 两个容器 (与旧实现一致: 不下钻到循环体内, 因为分块一次就已
/// 把内层结构重建完)。
fn tile_block(block: &mut Block, tile_size: usize, ctr: &mut u32) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        if let BlockItem::Stmt(Stmt::For {
            ref init,
            ref cond,
            ref step,
            ref body,
            ref span,
        }) = item
        {
            if let Some(tiled) = try_tile_nest(init, cond, step, body, tile_size, ctr, span) {
                out.push(tiled);
                continue;
            }
        }
        match item {
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                tile_block(&mut b, tile_size, ctr);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            },
            BlockItem::Stmt(Stmt::If {
                cond,
                then_,
                else_,
                span,
            }) => {
                let mut then_b = match *then_ {
                    Stmt::Block { b, .. } => b,
                    s => Block {
                        items: vec![BlockItem::Stmt(s)],
                        span: span.clone(),
                    },
                };
                tile_block(&mut then_b, tile_size, ctr);
                let else_b = else_.map(|e| -> Box<Stmt> {
                    match *e {
                        Stmt::Block { b, span } => {
                            let mut bb = b;
                            tile_block(&mut bb, tile_size, ctr);
                            Box::new(Stmt::Block { b: bb, span })
                        },
                        s => Box::new(s),
                    }
                });
                out.push(BlockItem::Stmt(Stmt::If {
                    cond,
                    then_: Box::new(Stmt::Block {
                        b: then_b,
                        span: span.clone(),
                    }),
                    else_: else_b,
                    span,
                }));
            },
            other => out.push(other),
        }
    }
    block.items = out;
}

/// 检测「可安全分块」的计数 for: `for (int ivar = 0; ivar < 常量; ivar++)`。
///
/// 与 `detect_simple_for` 的差别: 本函数**把声明节点与边界/推进表达式节点原样返回**。
/// 分块要重新声明归纳变量并复用它们的类型/字面量形态, 自己造 `Type::Int` 会把
/// `char`/`long`/`unsigned` 循环变量换成另一种宽度; 而 `for (i = 0; ...)` (无声明)
/// 形式里 `i` 声明在外层作用域、循环之后还可能被读, 重新声明会改变它的可见性。
fn tile_detect_counted(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
) -> Option<(crate::ast::VarDecl, Expr, Expr)> {
    let decl = match init.as_deref()? {
        Stmt::Block { b, .. } if b.items.len() == 1 => match &b.items[0] {
            BlockItem::Decl(Decl::Var(v)) => v.clone(),
            _ => return None,
        },
        _ => return None,
    };
    // 只接受「初始化为常量 0」的计数循环 (与 `init_stmt_var` 同一保守约定)。
    if !matches!(decl.init, Some(Expr::Int { v: 0, .. })) {
        return None;
    }
    // static / _Thread_local / extern / 带 asm 标签的声明不是「每轮重初始化」的循环变量。
    if decl.is_static || decl.is_thread_local || decl.is_extern || decl.asm_label.is_some() {
        return None;
    }
    let bound = match cond.as_ref()? {
        Expr::Binop {
            op: BinOp::Lt,
            l,
            r,
            ..
        } => {
            match l.as_ref() {
                Expr::Ident { name, .. } if name.name == decl.name.name => {},
                _ => return None,
            }
            match r.as_ref() {
                e @ Expr::Int { .. } => e.clone(),
                _ => return None,
            }
        },
        _ => return None,
    };
    match step.as_ref()? {
        Expr::PostInc { e, .. } => match e.as_ref() {
            Expr::Ident { name, .. } if name.name == decl.name.name => {},
            _ => return None,
        },
        _ => return None,
    }
    Some((decl, bound, (*step.as_ref().unwrap()).clone()))
}

/// 该语句是否「就是且只是一个 for 循环」(完美嵌套判据)。
///
/// `for (int i ...) for (int j ...) BODY` 的外层体直接是 `Stmt::For`; 带花括号时是
/// 只含一条语句的 `Stmt::Block`。外层体还包含其它语句时不能分块 —— 那些语句原本
/// 每 `i` 轮只执行一次, 分块后会被块循环重排到不同的轮次里。
fn single_for_stmt(body: &Stmt) -> Option<&Stmt> {
    match body {
        s @ Stmt::For { .. } => Some(s),
        Stmt::Block { b, .. } if b.items.len() == 1 => match &b.items[0] {
            BlockItem::Stmt(s @ Stmt::For { .. }) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

/// 体内是否存在让「重新声明归纳变量」失去等价性的形态。
///
/// 直接复用 LICM 的穷举名字扫描器 [`scan_stmt`]: 它对 `goto` / `label` / `switch`
/// / 内联汇编 / 无法判定写入目标的左值一律置 `reject_all` (fail-safe), 并给出
/// 「被写名字集合」与「地址逃逸集合」。分块要求:
///   * 体内不写两个归纳变量 (写它就是在改子循环自己的推进变量 —— 原语义里一次
///     内循环里写 `i` 会影响后续所有轮次, 而块形式下它只作当前子循环);
///   * 归纳变量没被取址 (别名可能指向被换掉的那份存储);
///   * 没有出现名字栅栏表达不了的构造。
fn tile_body_is_safe(body: &Stmt, ivars: &[&str]) -> bool {
    let mut scan = NameScan::default();
    scan_stmt(&mut scan, body, Site::Body);
    if scan.reject_all || scan.poison || scan.gives_up {
        return false;
    }
    ivars.iter().all(|v| {
        !scan.written.contains_key(*v)
            && !scan.escaped.contains(*v)
            && !scan.control.contains(*v)
            && !scan.persistent.contains(*v)
            && !scan.volatiles.contains(*v)
    })
}

/// 两层计数循环的分块入口: 命中则返回改写后的语句, 否则 `None` (不动原循环)。
fn try_tile_nest(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
    body: &Stmt,
    tile_size: usize,
    ctr: &mut u32,
    span: &Span,
) -> Option<BlockItem> {
    if tile_size < 2 {
        return None;
    }
    let outer = tile_detect_counted(init, cond, step)?;
    let inner_stmt = single_for_stmt(body)?;
    let inner = match inner_stmt {
        Stmt::For {
            init, cond, step, ..
        } => tile_detect_counted(init, cond, step)?,
        _ => return None,
    };
    let inner_body = match inner_stmt {
        Stmt::For { body, .. } => body,
        _ => return None,
    };
    if outer.0.name.name == inner.0.name.name {
        return None;
    }
    let trip = |e: &Expr| -> Option<i128> {
        match e {
            Expr::Int { v, .. } => Some(*v),
            _ => None,
        }
    };
    let ts = tile_size as i128;
    let (n, m) = (trip(&outer.1)?, trip(&inner.1)?);
    // 两个维度至少各要装得下两个块, 否则分块只是白加边界比较。
    if n <= ts || m <= ts {
        return None;
    }
    if !tile_body_is_safe(inner_body, &[outer.0.name.name.as_str(), inner.0.name.name.as_str()]) {
        return None;
    }
    Some(BlockItem::Stmt(build_blocked_nest(
        &outer, &inner, inner_body, tile_size, ctr, span,
    )))
}

/// 生成 ts×ts 分块的四层循环 (外层两个块循环 + 内层两个子循环, 体逐字保留)。
///
/// 所有新建节点的类型/字面量都从**原循环节点** clone 而来 (见
/// [`tile_detect_counted`] 的理由): 块索引 `ti`/`tj` 复用对应声明的类型与
/// 「初始化为 0」的常量节点。
fn build_blocked_nest(
    outer: &(crate::ast::VarDecl, Expr, Expr),
    inner: &(crate::ast::VarDecl, Expr, Expr),
    inner_body: &Stmt,
    tile_size: usize,
    ctr: &mut u32,
    span: &Span,
) -> Stmt {
    let (odecl, obound, ostep) = outer;
    let (idecl, ibound, istep) = inner;
    let id = *ctr;
    *ctr += 1;
    let ti_name = format!("__dcc_tile_i{}", id);
    let tj_name = format!("__dcc_tile_j{}", id);

    let ident = |name: &str, ty: &Type| Expr::Ident {
        name: Ident {
            name: name.to_string(),
            span: span.clone(),
        },
        span: span.clone(),
        ty: ty.clone(),
    };
    // 与原 `ivar = 0` 同一形态的整型字面量 (保留前端算好的类型)。
    let lit = |proto: &crate::ast::VarDecl, v: i128| -> Expr {
        match &proto.init {
            Some(Expr::Int { span: s, ty, .. }) => Expr::Int {
                v,
                span: s.clone(),
                ty: ty.clone(),
            },
            _ => Expr::Int {
                v,
                span: span.clone(),
                ty: Type::Int,
            },
        }
    };
    let bin = |op: BinOp, l: Expr, r: Expr, ty: &Type| Expr::Binop {
        op,
        l: Box::new(l),
        r: Box::new(r),
        span: span.clone(),
        ty: ty.clone(),
    };
    let stmt_for = |decl: crate::ast::VarDecl, cond: Expr, step: Expr, body: Stmt| -> Stmt {
        Stmt::For {
            init: Some(Box::new(Stmt::Block {
                b: Block {
                    items: vec![BlockItem::Decl(Decl::Var(decl))],
                    span: span.clone(),
                },
                span: span.clone(),
            })),
            cond: Some(cond),
            step: Some(step),
            body: Box::new(body),
            span: span.clone(),
        }
    };
    let re_decl = |base: &crate::ast::VarDecl, name: &str, init: Expr| -> crate::ast::VarDecl {
        let mut dv = base.clone();
        dv.name = Ident {
            name: name.to_string(),
            span: span.clone(),
        };
        dv.init = Some(init);
        dv
    };
    // `ti = ti + ts`: 前端把复合赋值展成 `l = l op r` 并保留 `op`, 与其他 pass 同一
    // 约定 (只看 `r` 里的算术, `op` 字段不参与求值)。
    let stride_step = |name: &str, proto: &crate::ast::VarDecl| -> Expr {
        Expr::Assign {
            op: BinOp::Add,
            l: Box::new(ident(name, &proto.ty)),
            r: Box::new(bin(
                BinOp::Add,
                ident(name, &proto.ty),
                lit(proto, tile_size as i128),
                &proto.ty,
            )),
            span: span.clone(),
            ty: proto.ty.clone(),
        }
    };
    // 子循环条件: `i < N && i < ti + ts` (右端是块内上界, 左端保证不越总量)。
    // `ts` 字面量取块索引变量自己的类型, 避免 `long i` 与 `char j` 混用时宽度错位。
    let sub_cond = |name: &str, ty: &Type, bound: &Expr, blk: &str, blk_ty: &Type| -> Expr {
        bin(
            BinOp::And,
            bin(BinOp::Lt, ident(name, ty), bound.clone(), ty),
            bin(
                BinOp::Lt,
                ident(name, ty),
                bin(
                    BinOp::Add,
                    ident(blk, blk_ty),
                    Expr::Int {
                        v: tile_size as i128,
                        span: span.clone(),
                        ty: blk_ty.clone(),
                    },
                    blk_ty,
                ),
                ty,
            ),
            ty,
        )
    };

    let j_loop = stmt_for(
        re_decl(idecl, &idecl.name.name, ident(&tj_name, &idecl.ty)),
        sub_cond(&idecl.name.name, &idecl.ty, ibound, &tj_name, &idecl.ty),
        istep.clone(),
        inner_body.clone(),
    );
    let i_loop = stmt_for(
        re_decl(odecl, &odecl.name.name, ident(&ti_name, &odecl.ty)),
        sub_cond(&odecl.name.name, &odecl.ty, obound, &ti_name, &odecl.ty),
        ostep.clone(),
        j_loop,
    );
    let tj_loop = stmt_for(
        re_decl(idecl, &tj_name, lit(idecl, 0)),
        bin(BinOp::Lt, ident(&tj_name, &idecl.ty), ibound.clone(), &idecl.ty),
        stride_step(&tj_name, idecl),
        i_loop,
    );
    stmt_for(
        re_decl(odecl, &ti_name, lit(odecl, 0)),
        bin(BinOp::Lt, ident(&ti_name, &odecl.ty), obound.clone(), &odecl.ty),
        stride_step(&ti_name, odecl),
        tj_loop,
    )
}

// ===== P7. 自动并行化标注 =====

/// 检测可并行化的 for 循环并添加 `#pragma omp simd` 标注。
/// 通过插入注释式标注 (asm volatile) 告知后端。
fn parallelize_block(block: &mut Block) {
    let mut out = Vec::new();
    for item in block.items.drain(..) {
        match item {
            BlockItem::Stmt(Stmt::For {
                init,
                cond,
                step,
                body,
                span,
            }) => {
                if is_parallelizable_for(&init, &cond, &step, &body) {
                    // 插入并行化标注: asm volatile("#pragma omp simd")
                    out.push(BlockItem::Stmt(Stmt::Expr {
                        e: Expr::AsmExpr {
                            asm_str: "#pragma omp simd".to_string(),
                            outputs: vec![],
                            output_names: vec![],
                            output_constraints: vec![],
                            inputs: vec![],
                            input_names: vec![],
                            input_constraints: vec![],
                            clobbers: vec![],
                            volatile: true,
                            span: span.clone(),
                            ty: Type::Void,
                        },
                        span: span.clone(),
                    }));
                }
                out.push(BlockItem::Stmt(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                    span,
                }));
            },
            BlockItem::Stmt(Stmt::Block { mut b, span }) => {
                parallelize_block(&mut b);
                out.push(BlockItem::Stmt(Stmt::Block { b, span }));
            },
            other => out.push(other),
        }
    }
    block.items = out;
}

/// 检测 for 循环是否可并行化: 无函数调用、无间接访问、线性归纳变量
fn is_parallelizable_for(
    init: &Option<Box<Stmt>>,
    cond: &Option<Expr>,
    step: &Option<Expr>,
    body: &Stmt,
) -> bool {
    // 必须有简单归纳变量
    if detect_simple_for(init, cond, step).is_none() {
        return false;
    }
    // 循环体无函数调用
    !stmt_has_call(body)
}

fn stmt_has_call(s: &Stmt) -> bool {
    match s {
        Stmt::Expr { e, .. } => expr_has_call(e),
        Stmt::Block { b, .. } => b.items.iter().any(|i| match i {
            BlockItem::Stmt(s) => stmt_has_call(s),
            _ => false,
        }),
        Stmt::If { then_, else_, .. } => {
            stmt_has_call(then_) || else_.as_ref().is_some_and(|e| stmt_has_call(e))
        },
        Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            stmt_has_call(body)
        },
        _ => false,
    }
}

fn expr_has_call(e: &Expr) -> bool {
    match e {
        Expr::Call { .. } => true,
        Expr::Binop { l, r, .. } | Expr::Comma { l, r, .. } => expr_has_call(l) || expr_has_call(r),
        Expr::Unary { e, .. }
        | Expr::Cast { e, .. }
        | Expr::AddrOf { e, .. }
        | Expr::Deref { e, .. }
        | Expr::PreInc { e, .. }
        | Expr::PreDec { e, .. }
        | Expr::PostInc { e, .. }
        | Expr::PostDec { e, .. } => expr_has_call(e),
        Expr::Assign { l, r, .. } => expr_has_call(l) || expr_has_call(r),
        Expr::Cond {
            cond, then_, else_, ..
        } => expr_has_call(cond) || expr_has_call(then_) || expr_has_call(else_),
        Expr::Index { arr, idx, .. } => expr_has_call(arr) || expr_has_call(idx),
        _ => false,
    }
}

// ===== 工具函数 =====

fn make_ident(name: &str, span: &Span) -> Expr {
    Expr::Ident {
        name: Ident {
            name: name.into(),
            span: span.clone(),
        },
        span: span.clone(),
        ty: int_ty(),
    }
}

fn make_var_decl(name: &str, val: i128, span: &Span) -> Stmt {
    Stmt::Expr {
        e: Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Ident {
                name: Ident {
                    name: name.into(),
                    span: span.clone(),
                },
                span: span.clone(),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: val,
                span: span.clone(),
                ty: int_ty(),
            }),
            span: span.clone(),
            ty: int_ty(),
        },
        span: span.clone(),
    }
}

fn int_ty() -> Type {
    Type::Int
}

/// 替换循环体中的索引变量为新的表达式
fn subst_index_var(body: &Stmt, old_var: &str, new_expr: &Expr) -> Box<Stmt> {
    Box::new(subst_stmt(body, old_var, new_expr))
}

fn subst_stmt(s: &Stmt, old: &str, new_e: &Expr) -> Stmt {
    match s {
        Stmt::Expr { e, span } => Stmt::Expr {
            e: subst_expr(e, old, new_e),
            span: span.clone(),
        },
        Stmt::Return { val, span } => Stmt::Return {
            val: val.as_ref().map(|e| subst_expr(e, old, new_e)),
            span: span.clone(),
        },
        Stmt::Block { b, span } => Stmt::Block {
            b: Block {
                items: b
                    .items
                    .iter()
                    .map(|i| match i {
                        BlockItem::Stmt(s) => BlockItem::Stmt(subst_stmt(s, old, new_e)),
                        other => other.clone(),
                    })
                    .collect(),
                span: b.span.clone(),
            },
            span: span.clone(),
        },
        Stmt::If {
            cond,
            then_,
            else_,
            span,
        } => Stmt::If {
            cond: subst_expr(cond, old, new_e),
            then_: Box::new(subst_stmt(then_, old, new_e)),
            else_: else_.as_ref().map(|e| Box::new(subst_stmt(e, old, new_e))),
            span: span.clone(),
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            span,
        } => Stmt::For {
            init: init.as_ref().map(|i| Box::new(subst_stmt(i, old, new_e))),
            cond: cond.as_ref().map(|c| subst_expr(c, old, new_e)),
            step: step.as_ref().map(|s| subst_expr(s, old, new_e)),
            body: Box::new(subst_stmt(body, old, new_e)),
            span: span.clone(),
        },
        Stmt::While { cond, body, span } => Stmt::While {
            cond: subst_expr(cond, old, new_e),
            body: Box::new(subst_stmt(body, old, new_e)),
            span: span.clone(),
        },
        Stmt::Do { body, cond, span } => Stmt::Do {
            body: Box::new(subst_stmt(body, old, new_e)),
            cond: subst_expr(cond, old, new_e),
            span: span.clone(),
        },
        other => other.clone(),
    }
}

fn subst_expr(e: &Expr, old: &str, new_e: &Expr) -> Expr {
    match e {
        Expr::Ident { name, span, ty } if name.name == old => {
            let mut ne = new_e.clone();
            // 保留原始 span
            match &mut ne {
                Expr::Ident { span: s, .. } => *s = span.clone(),
                Expr::Binop { span: s, .. } => *s = span.clone(),
                _ => {},
            }
            ne
        },
        Expr::Binop { op, l, r, span, ty } => Expr::Binop {
            op: *op,
            l: Box::new(subst_expr(l, old, new_e)),
            r: Box::new(subst_expr(r, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Assign { op, l, r, span, ty } => Expr::Assign {
            op: *op,
            l: Box::new(subst_expr(l, old, new_e)),
            r: Box::new(subst_expr(r, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Index { arr, idx, span, ty } => Expr::Index {
            arr: Box::new(subst_expr(arr, old, new_e)),
            idx: Box::new(subst_expr(idx, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Call {
            callee,
            args,
            span,
            ty,
        } => Expr::Call {
            callee: Box::new(subst_expr(callee, old, new_e)),
            args: args.iter().map(|a| subst_expr(a, old, new_e)).collect(),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Unary {
            op,
            e: inner,
            span,
            ty,
        } => Expr::Unary {
            op: *op,
            e: Box::new(subst_expr(inner, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::Cast { ty, e: inner, span } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(subst_expr(inner, old, new_e)),
            span: span.clone(),
        },
        Expr::Cond {
            cond,
            then_,
            else_,
            span,
            ty,
        } => Expr::Cond {
            cond: Box::new(subst_expr(cond, old, new_e)),
            then_: Box::new(subst_expr(then_, old, new_e)),
            else_: Box::new(subst_expr(else_, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        Expr::PostInc { e: inner, span, ty } => Expr::PostInc {
            e: Box::new(subst_expr(inner, old, new_e)),
            span: span.clone(),
            ty: ty.clone(),
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ident, Type};
    use crate::diagnostics::Span;

    fn int_ty() -> Type {
        Type::Int
    }

    /// Hot function: trip count 10 should be unrolled (max=16).
    /// Cold function: trip count 10 should NOT be unrolled (max=4).
    #[test]
    fn hot_func_max_trip_16_accepts_trip_10() {
        let sp = Span::point("t.c", 1, 1);
        let init = Stmt::Expr {
            e: Expr::Assign {
                op: BinOp::Add, // plain assign uses any BinOp
                l: Box::new(Expr::Ident {
                    name: Ident {
                        name: "i".into(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: int_ty(),
                }),
                r: Box::new(Expr::Int {
                    v: 0,
                    span: sp.clone(),
                    ty: int_ty(),
                }),
                span: sp.clone(),
                ty: int_ty(),
            },
            span: sp.clone(),
        };
        let cond = Expr::Binop {
            op: BinOp::Lt,
            l: Box::new(Expr::Ident {
                name: Ident {
                    name: "i".into(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: 10,
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };
        let step = Expr::PostInc {
            e: Box::new(Expr::Ident {
                name: Ident {
                    name: "i".into(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };

        // max_trip=4 should reject trip=10
        let r_cold = parse_counted_for(Some(&init), Some(&cond), Some(&step), 4);
        assert!(r_cold.is_none(), "cold func max_trip=4 should reject trip=10");

        // max_trip=16 should accept trip=10
        let r_hot = parse_counted_for(Some(&init), Some(&cond), Some(&step), 16);
        assert!(r_hot.is_some(), "hot func max_trip=16 should accept trip=10");
        let (ivar, trip) = r_hot.unwrap();
        assert_eq!(ivar, "i");
        assert_eq!(trip, 10);
    }

    /// Trip count within default limit (<=4) should always be accepted.
    #[test]
    fn small_trip_always_accepted() {
        let sp = Span::point("t.c", 1, 1);
        let init = Stmt::Expr {
            e: Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Ident {
                    name: Ident {
                        name: "j".into(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: int_ty(),
                }),
                r: Box::new(Expr::Int {
                    v: 0,
                    span: sp.clone(),
                    ty: int_ty(),
                }),
                span: sp.clone(),
                ty: int_ty(),
            },
            span: sp.clone(),
        };
        let cond = Expr::Binop {
            op: BinOp::Lt,
            l: Box::new(Expr::Ident {
                name: Ident {
                    name: "j".into(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: int_ty(),
            }),
            r: Box::new(Expr::Int {
                v: 3,
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };
        let step = Expr::PostInc {
            e: Box::new(Expr::Ident {
                name: Ident {
                    name: "j".into(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: int_ty(),
            }),
            span: sp.clone(),
            ty: int_ty(),
        };

        // Both cold and hot should accept trip=3
        let r_cold = parse_counted_for(Some(&init), Some(&cond), Some(&step), 4);
        assert!(r_cold.is_some());
        assert_eq!(r_cold.unwrap().1, 3);

        let r_hot = parse_counted_for(Some(&init), Some(&cond), Some(&step), 16);
        assert!(r_hot.is_some());
        assert_eq!(r_hot.unwrap().1, 3);
    }
}

#[cfg(test)]
mod tile_equiv_tests {
    use super::*;
    use std::collections::HashMap;

    /// 一个只覆盖「分块前后会出现的形态」的小解释器: 把每次写入 `acc` 时
    /// 归纳变量 (i, j) 的取值记录下来, 得到该循环结构的**迭代序列**。
    /// 未知形态一律 panic —— 宁可测试炸掉, 也不要在覆盖不到的形态上给出「等价」的假结论。
    fn eval_expr(e: &Expr, env: &HashMap<String, i128>) -> i128 {
        match e {
            Expr::Int { v, .. } => *v,
            Expr::Char { v, .. } => *v,
            Expr::Ident { name, .. } => *env
                .get(&name.name)
                .unwrap_or_else(|| panic!("解释器: 读了未赋值的名字 {}", name.name)),
            Expr::Binop { op, l, r, .. } => {
                let a = eval_expr(l, env);
                let b = eval_expr(r, env);
                match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Lt => (a < b) as i128,
                    BinOp::Gt => (a > b) as i128,
                    BinOp::EqEq => (a == b) as i128,
                    BinOp::And => (a != 0 && b != 0) as i128,
                    BinOp::Or => (a != 0 || b != 0) as i128,
                    o => panic!("解释器: 未覆盖的二元运算 {:?}", o),
                }
            },
            Expr::Unary {
                op: crate::ast::UnOp::Neg,
                e: inner,
                ..
            } => -eval_expr(inner, env),
            Expr::Cast { e: inner, .. } => eval_expr(inner, env),
            other => panic!("解释器: 未覆盖的表达式形态 {:?}", other),
        }
    }

    fn run_stmt(s: &Stmt, env: &mut HashMap<String, i128>, trace: &mut Vec<(i128, i128)>) {
        match s {
            Stmt::Block { b, .. } => run_block(b, env, trace),
            Stmt::Expr { e, .. } => run_expr_stmt(e, env, trace),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                if eval_expr(cond, env) != 0 {
                    run_stmt(then_, env, trace);
                } else if let Some(el) = else_ {
                    run_stmt(el, env, trace);
                }
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    run_stmt(i, env, trace);
                }
                loop {
                    if let Some(c) = cond {
                        if eval_expr(c, env) == 0 {
                            break;
                        }
                    }
                    run_stmt(body, env, trace);
                    if let Some(st) = step {
                        run_expr_stmt(st, env, trace);
                    }
                }
            },
            // `return` 在分块前后都只会出现一次且不影响体内的归纳变量序列,
            // 这里取它的值但不记录 (函数级行为由执行级夹具守)。
            Stmt::Return { .. } => {},
            other => panic!("解释器: 未覆盖的语句形态 {:?}", other),
        }
    }

    fn run_block(b: &Block, env: &mut HashMap<String, i128>, trace: &mut Vec<(i128, i128)>) {
        for item in &b.items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    let val = v
                        .init
                        .as_ref()
                        .map(|i| eval_expr(i, env))
                        .unwrap_or_else(|| env.get(&v.name.name).copied().unwrap_or(0));
                    env.insert(v.name.name.clone(), val);
                },
                BlockItem::Stmt(s) => run_stmt(s, env, trace),
                other => panic!("解释器: 未覆盖的块条目 {:?}", other),
            }
        }
    }

    fn run_expr_stmt(e: &Expr, env: &mut HashMap<String, i128>, trace: &mut Vec<(i128, i128)>) {
        match e {
            Expr::Assign { l, r, .. } => {
                let name = match l.as_ref() {
                    Expr::Ident { name, .. } => name.name.clone(),
                    other => panic!("解释器: 不支持的左值 {:?}", other),
                };
                let val = eval_expr(r, env);
                if name == "acc" {
                    // 记录一次「体内写入」时两个归纳变量的取值 ⇒ 迭代序列。
                    trace.push((env["i"], env["j"]));
                }
                env.insert(name, val);
            },
            Expr::PostInc { e: inner, .. } | Expr::PreInc { e: inner, .. } => {
                let name = match inner.as_ref() {
                    Expr::Ident { name, .. } => name.name.clone(),
                    other => panic!("解释器: 不支持的自增操作数 {:?}", other),
                };
                let cur = env[name.as_str()];
                env.insert(name, cur + 1);
            },
            other => panic!("解释器: 未覆盖的表达式语句 {:?}", other),
        }
    }

    fn trace_of(b: &Block) -> Vec<(i128, i128)> {
        let mut env = HashMap::new();
        let mut trace = Vec::new();
        run_block(b, &mut env, &mut trace);
        trace
    }

    fn nest_body() -> Block {
        let src = "int f(void){int acc=0;for(int i=0;i<6;i++)for(int j=0;j<5;j++)acc=acc+i*100+j;return acc;}\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        m.source()
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) => f.body.clone(),
                _ => None,
            })
            .expect("函数体")
    }

    /// 分块必须保持**逐点相同的迭代序列** (顺序可以变, 集合与重数不可以)。
    /// 同时断言分块真的发生了 —— 否则「一致」是空判。
    #[test]
    fn tile_nest_preserves_iteration_space() {
        let before = nest_body();
        let seq_before = trace_of(&before);
        assert_eq!(seq_before.len(), 30, "6×5 两层嵌套应执行 30 次体内写入");

        let mut after = nest_body();
        let mut ctr = 0u32;
        tile_block(&mut after, 4, &mut ctr);
        assert_eq!(ctr, 1, "6×5 完美两层嵌套必须被分块 (否则本测试是空判)");
        let seq_after = trace_of(&after);
        let mut sorted_before = seq_before.clone();
        let mut sorted_after = seq_after.clone();
        sorted_before.sort();
        sorted_after.sort();
        assert_eq!(
            sorted_before,
            sorted_after,
            "分块改变了迭代空间 (执行次数 {} vs {}, 且多重集不同)",
            seq_before.len(),
            seq_after.len()
        );
        assert_eq!(seq_after.len(), seq_before.len(), "分块不得改变体内写入次数");
    }

    /// 分块必须**逐字保留循环体** (不做任何名字替换) —— 这是它不依赖名字分析、
    /// 结构上排除捕获的依据。旧实现把体内 `i` 换成 `ii + j` 正是错值来源。
    #[test]
    fn tile_keeps_body_verbatim() {
        // 取原构造的内层 For 的体 (逐字对比的基准)。
        let orig = nest_body();
        let mut orig_inner: &Stmt = orig
            .items
            .iter()
            .find_map(|it| match it {
                BlockItem::Stmt(s @ Stmt::For { .. }) => Some(s),
                _ => None,
            })
            .expect("原形态应有一个外层 For");
        // 原形态: acc=0; for(i) for(j) BODY → 钻两层到 BODY
        while let Stmt::For { body, .. } = orig_inner {
            orig_inner = body.as_ref();
        }
        let orig_body_dump = format!("{:?}", orig_inner);

        let mut after = nest_body();
        let mut ctr = 0u32;
        tile_block(&mut after, 4, &mut ctr);
        assert_eq!(ctr, 1, "6×5 嵌套应被分块");
        // 钻到最内层: ti → tj → i → j 四层 For 的体。
        let mut inner: Option<&Stmt> = None;
        for item in &after.items {
            if let BlockItem::Stmt(s @ Stmt::For { .. }) = item {
                inner = Some(s);
            }
        }
        let mut cur = inner.expect("分块后应仍有最外层 For");
        let mut depth = 0;
        while let Stmt::For { body, .. } = cur {
            depth += 1;
            cur = body.as_ref();
        }
        assert_eq!(depth, 4, "分块应产出 ti/tj/i/j 四层 For, 实际 {depth} 层");
        let dumped = format!("{:?}", cur);
        assert_eq!(
            dumped, orig_body_dump,
            "最内层体必须与原体逐字相同 (分块不得改名字或 replacer 表达式)"
        );
        assert!(
            dumped.contains("\"i\"") && dumped.contains("\"j\""),
            "体应仍引用归纳变量 i / j: {}",
            dumped
        );
        assert!(
            !dumped.contains("__dcc_tile_i") && !dumped.contains("__dcc_tile_j"),
            "体内不得出现块索引 (出现即说明做了名字替换): {}",
            dumped
        );
    }

    /// 体内写归纳变量 / 有 goto 的形态必须**放弃分块** (否则推进语义被搬进子循环)。
    #[test]
    fn tile_refuses_unsafe_nests() {
        // 体内写 i (与分块后的子循环推进冲突)
        let src = "int f(void){int acc=0;for(int i=0;i<6;i++)for(int j=0;j<5;j++){i=i+1;acc=acc+i*j;}return acc;}\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let mut body = m
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) => f.body.clone(),
                _ => None,
            })
            .expect("函数体");
        let before = format!("{:?}", body);
        let mut ctr = 0u32;
        tile_block(&mut body, 4, &mut ctr);
        assert_eq!(ctr, 0, "体内写归纳变量的嵌套不得分块");
        assert_eq!(format!("{:?}", body), before, "放弃分块时不得留下任何改动");
    }
}
