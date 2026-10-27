// src/frame_guard.rs — 「帧槽事实 / 寄存器家能不能跨过一处控制流」的唯一判据
//
// 为什么必须有唯一一个地方回答它（方案 §4.7 的原文要求：「S2 的四个清槽条件必须与既有
// addr_escaped / returns_twice / VLA / asm 判定同源，不许另写一份」）：
//
// 这三条消费者本来各自长在三个地方，谁也不知道别人存在 ——
//   * `opt.rs` 的 returns_twice 影响域（整函数退出 SSA 往返）；
//   * `ssa/regalloc.rs` 的家绑定（`addr_escaped_names` / `volatile_touched_names` / asm 操作数）；
//   * `codegen/x86_64/blockopt.rs` 的块内事实（`volatile_frames` 那一套偏移枚举）。
// 一旦把「事实跨调用存活」（S2 A1）或「区内家」（S4）接上去，任何一处**少写一个条件**
// 得到的都不是「少削一条」而是静默错值。所以本文件只做一件事：把这几条既有判据**汇成
// 一个结构体**，三个消费者都读同一个答案。本文件不重新发明任何判据 —— 每个字段都由
// 既有真源算出（下面每条都写明它从哪来）。
//
// 方向纪律：所有字段都是「命中 ⇒ 关掉一侧优化」的**保守**方向。判据算错成 true 只是
// 丢优化，算错成 false 就是错值，因此每一项都用最宽的既有判据，宁宽勿窄。

use crate::ast::{Block, BlockItem, Decl, Expr, FuncDecl, Stmt};
use std::collections::{HashMap, HashSet};

/// 二次返回（returns-twice）名字族。
///
/// **这里就是唯一真源**：`opt.rs` 的 SSA 影响域、`body_is_splicable` 的内联闸门、本文件的
/// 护栏全部读它。旧版 `opt.rs::KNOWN_RETURNS_TWICE` 少两个条目（方案 §4.1 指出的缺口）：
/// `__builtin_setjmp` / `__builtin_longjmp` 是 dcc 自己实现的 builtin，它们的快照只存
/// rbx/r12/r14/r15/rbp/r13 —— 也就是说**只有 callee-saved 号上的家能被它恢复**。今天
/// 绑在 r8/r9（`regalloc::AUTO_EXT_POOL`）上的家跨一次 `__builtin_longjmp` 就是错值，
/// 因为那条 builtin 既不保存也不恢复它们。
///
/// 为什么不把 libc 的 `longjmp` 一族也塞进这张表：影响域是**向上**传播的（调用者及其
/// 传递 caller 全部退出 SSA）。`luaD_throw → longjmp` 会把 `luaG_runerror`、`luaV_execute`
/// 整条链拉出 SSA，于是解释器丢掉全部寄存器家 —— 那是比原缺陷严重得多的倒退。libc 的
/// `setjmp` 会保存全部 callee-saved 号，而含它的函数已经在影响域里连家都没有，所以
/// libc 路径本来就不需要这条额外的护栏；需要护栏的只有 dcc 自己实现的那对 builtin。
pub const RETURNS_TWICE_NAMES: &[&str] = &[
    "setjmp",
    "_setjmp",
    "__setjmp",
    "__sigsetjmp",
    "sigsetjmp",
    "savectx",
    "savectx_rt",
    "vfork",
    "getcontext",
    "setcontext",
    // ↓ 方案 §4.1 补的两条：dcc builtin 的 setjmp/longjmp 对。
    "__builtin_setjmp",
    "__builtin_longjmp",
];

/// 本函数**自己**含二次返回调用点的判据。与 `opt.rs` 的影响域不同：影响域问的是
/// 「整条链要不要退出 SSA」，这里问的是「这一帧的恢复点在哪」——只有自身含调用点的
/// 函数才会被非局部跳转**落入**，因此 caller-saved 家的护栏只看本函数。
pub fn self_has_returns_twice(f: &FuncDecl) -> bool {
    let names: HashSet<String> = RETURNS_TWICE_NAMES.iter().map(|s| s.to_string()).collect();
    crate::opt::func_contains_rt_call(f, &names)
}

/// 一次函数级护栏的结论。字段名就是「哪一种控制流形状要挡住事实」。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameGuard {
    /// 本函数含 returns-twice 调用点（二次返回的汇合点在函数内部）。
    pub returns_twice: bool,
    /// 本函数的某个局部/形参被取过地址（callee 可能拿着那个指针写回本帧）。
    ///
    /// 为什么还要函数级这一位，而不是只靠下面那张名字集：A1 的「callee 写不到本帧」
    /// 是个全局命题（一次 call 可以写**任何**已逃逸对象），影响域、内联闸门等旧消费者
    /// 读的都是这个粗粒度答案。细粒度的逐槽判断走 `escaped_names` → 发射侧的
    /// `Codegen::escaped_frames`。
    pub addr_escaped: bool,
    /// 被取过址的**本函数局部/形参名**集（已名集）。发射侧据此把具体帧槽登记成
    /// 「不可推迟存出 / 不可跨 call 存活」的槽。
    ///
    /// 为什么不能只靠 `addr_escaped` 那一位：Lua 的 `luaV_execute` 里 `&base[i]` 这类
    /// 「由指针变量算出的地址」会被 `addr_escaped_names` 归到根名 `base` 上，于是整个函数
    /// 被标成逃逸——而 `base` 自己的槽（一个指针值）根本没有逃逸。粗粒度是安全的，但它
    /// 把 S4 在目标负载上直接关死（实测：按函数判定时 `luaV_execute` 一个家都不提升）。
    /// callee 能写的只是 `base[i]` 指的那个对象，不是 `base` 自己——所以家与 A1 都按槽判。
    pub escaped_names: HashSet<String>,
    /// 本函数有 VLA：栈帧随变量长度变化，帧槽的字节范围在编译期不完整可知。
    pub vla: bool,
    /// 本函数有内联 asm（表达式形、`asm goto`、带操作数的 `asm`）：asm 可以按任意偏移
    /// 访存，事实表看不见它的写集。
    pub inline_asm: bool,
    /// 本函数碰到 volatile 对象。真源是 `opt.rs::func_refs_volatile`（它读 AST 的 quals
    /// 与 volatile 全局名集），不是「声明处 quals」那一条 —— 后者在多条解析路径上恒为
    /// `NONE`（`regalloc.rs` 的形参家那段已经实测登记过这件事）。
    pub volatile_obj: bool,
    /// 本函数含嵌套函数定义：嵌套体按**外层帧偏移**访问外层变量，事实表没有那个视野。
    pub nested_fn: bool,
    /// 变参函数：`va_start` 在**本帧**里建寄存器保存区与溢出区，`va_arg` 经指针读它们。
    /// 那部分读写不走 `[r13+disp]` 的局部表，推迟存出会让 `va_arg` 读到旧值。
    pub variadic: bool,
}

impl FrameGuard {
    /// 逐函数算一次。全部字段来自既有真源，见每个字段的注释。
    pub fn for_func(f: &FuncDecl, volatile_globals: &HashSet<String>) -> FrameGuard {
        let body = match &f.body {
            Some(b) => b,
            None => return FrameGuard::default(),
        };
        let locals = local_names(f);
        // `addr_escaped_names` 返回**所有**被取址的根对象名（含全局）。全局取址不影响
        // 我们的帧槽，所以与本轮函数的局部名集求交，避免「文件里有个 `&g`」就把整条
        //  relaxation 关掉。
        let escaped = crate::ast_gvn::addr_escaped_names(body);
        let escaped_local: HashSet<String> = escaped
            .iter()
            .filter(|n| locals.contains(*n))
            .cloned()
            .collect();
        let g = FrameGuard {
            returns_twice: self_has_returns_twice(f),
            addr_escaped: !escaped_local.is_empty(),
            escaped_names: escaped_local,
            vla: has_vla_local(f),
            inline_asm: block_has_asm(body),
            volatile_obj: crate::opt::func_refs_volatile(f, volatile_globals),
            nested_fn: block_has_nested_fn(body),
            variadic: f.is_variadic,
        };
        g
    }

    /// **S2 A1 的四条件**（方案 §3 A1：「帧槽只在 returns_twice / addr_escaped / VLA /
    /// 内联 asm 时清」）。这里就是那四条的唯一落点，`blockopt` 不许自己再拼一遍。
    pub fn slots_may_survive_call(&self) -> bool {
        !(self.returns_twice || self.addr_escaped || self.vla || self.inline_asm)
    }

    /// 能不能把**caller-saved** 号（r8/r9 延伸池）当变量家。
    ///
    /// 只有 callee-saved 号会出现在二次返回的恢复快照里，所以本函数只要二次返回点在
    /// 自己身上就必须拒绝：`__builtin_longjmp` 落回汇合点时，r8/r9 上的家没有任何地方
    /// 把它恢复回来（方案 §4.1 的原文缺陷）。
    pub fn allows_caller_saved_home(&self) -> bool {
        !self.returns_twice
    }

    /// **S4 区域家**的函数级硬互斥（方案 §3 S4：「`-g`、任意内联 asm、`volatile`
    /// （含 pointee）、nested、`addr_escaped`、`-O0`」）。`-g` 与 `-O0` 不是函数体形状，
    /// 由调用方在传入前先判；变参也不是体形状但同属「本帧有指针看不见的读写」。
    ///
    /// `addr_escaped` 不在下面这一列：它不是函数级的开关而是**槽级**的（见
    /// `escaped_names` 那条注释），把它当函数级会把 S4 在目标负载上直接关死。
    pub fn blocks_region_home(&self) -> bool {
        self.inline_asm
            || self.volatile_obj
            || self.nested_fn
            || self.returns_twice
            || self.vla
            || self.variadic
    }
}

/// 全 TU 的 volatile 全局名集。
///
/// 这张表以前在 `opt.rs::run_ssa_passes` 里就地拼了一份。本函数把它收成一个真源：
/// SSA 准入与发射侧的护栏必须对「哪个名字是 volatile 全局」给同一个答案，否则
/// `-O2`（走 SSA）与 `-O0`（直接发射）对同一个形状会做出不同的保守决定 —— 而那正是
/// 四档执行必须同结果的口径。`compile_func` 拿得到 `&Program`，所以两侧能共用。
pub fn volatile_global_names(prog: &crate::ast::Program) -> HashSet<String> {
    prog.decls
        .iter()
        .filter_map(|d| match d {
            Decl::Var(v) if v.quals.is_volatile() => Some(v.name.name.clone()),
            _ => None,
        })
        .collect()
}

/// 本函数里「指向某个标签的非落空边」的完整账。
///
/// 这个计数是 A2 两臂共同的先验真源：
///   * `count(L) == 0` ⇒ L 的唯一前驱就是落空，事实照旧全留（A2 落空继承那一臂）；
///   * `count(L) == 已经数到的前驱快照数` ⇒ L 的全部前驱边都已经发完了，取交（A2 多前驱取交那一臂）；
///   * 其余（前驱边还没发完，例如循环回跳）⇒ 整体清。
pub struct JumpEdges {
    pub counts: HashMap<String, u32>,
}

impl Default for JumpEdges {
    fn default() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }
}

impl JumpEdges {
    fn add(&mut self, name: &str) {
        *self.counts.entry(name.to_string()).or_insert(0) += 1;
    }
    /// 指向 `name` 的边数（没出现过 = 0）。
    pub fn count(&self, name: &str) -> u32 {
        self.counts.get(name).copied().unwrap_or(0)
    }
}

/// 见 [`JumpEdges`]。这一份是 A2 两臂共同的唯一真源（不得另写一份边集）。
pub fn jump_edges(f: &FuncDecl) -> JumpEdges {
    let mut out = JumpEdges::default();
    if let Some(b) = &f.body {
        block_jump_targets(b, &mut out);
    }
    out
}

fn block_jump_targets(b: &Block, out: &mut JumpEdges) {
    for it in &b.items {
        match it {
            BlockItem::Stmt(s) => stmt_jump_targets(s, out),
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &f.body {
                    block_jump_targets(body, out);
                }
            },
            _ => {},
        }
    }
}

fn expr_jump_targets(e: &Expr, out: &mut JumpEdges) {
    use Expr as E;
    match e {
        // `&&label` 就是一条看不见的边的入口：它能被 `goto *表[…]` 在任何时刻落入。
        E::LabelAddr { label, .. } => {
            out.add(&label.name);
        },
        E::Int { .. }
        | E::Float { .. }
        | E::ComplexLit { .. }
        | E::ComplexIntLit { .. }
        | E::String { .. }
        | E::Char { .. }
        | E::Ident { .. }
        | E::SizeofTy { .. }
        | E::AlignofTy { .. }
        | E::VaArgPack { .. } => {},
        E::Real { e, .. }
        | E::Imag { e, .. }
        | E::Unary { e, .. }
        | E::Cast { e, .. }
        | E::SizeofExpr { e, .. }
        | E::AlignofExpr { e, .. }
        | E::AddrOf { e, .. }
        | E::Deref { e, .. }
        | E::PostInc { e, .. }
        | E::PostDec { e, .. }
        | E::PreInc { e, .. }
        | E::PreDec { e, .. }
        | E::Member { expr: e, .. }
        | E::VaArg { ap: e, .. } => expr_jump_targets(e, out),
        E::Binop { l, r, .. } | E::Assign { l, r, .. } | E::Comma { l, r, .. } => {
            expr_jump_targets(l, out);
            expr_jump_targets(r, out);
        },
        E::Index { arr, idx, .. } => {
            expr_jump_targets(arr, out);
            expr_jump_targets(idx, out);
        },
        E::Cond {
            cond, then_, else_, ..
        } => {
            expr_jump_targets(cond, out);
            expr_jump_targets(then_, out);
            expr_jump_targets(else_, out);
        },
        E::Call { callee, args, .. } => {
            expr_jump_targets(callee, out);
            for a in args {
                expr_jump_targets(a, out);
            }
        },
        E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
            for i in inits {
                match i {
                    crate::ast::InitItem::Expr(e) => expr_jump_targets(e, out),
                    crate::ast::InitItem::Designated { value, .. } => expr_jump_targets(value, out),
                }
            }
        },
        E::AsmExpr {
            outputs, inputs, ..
        } => {
            for x in outputs.iter().chain(inputs.iter()) {
                expr_jump_targets(x, out);
            }
        },
        E::StmtExpr { block, .. } => block_jump_targets(block, out),
        E::Offsetof { indices, .. } => {
            for i in indices {
                expr_jump_targets(i, out);
            }
        },
    }
}

fn stmt_jump_targets(s: &Stmt, out: &mut JumpEdges) {
    match s {
        Stmt::Goto { target, .. } => {
            out.add(&target.name);
        },
        // `goto *expr`：目标集不在名字里（运行时才知道）。本函数**不**返回「有间接跳转」
        // 这个信号，因为调用方需要的只是名字集；间接跳转的存在与否由
        // [`has_unnamed_jump`] 单独回答。
        Stmt::GotoIndirect { expr, .. } => expr_jump_targets(expr, out),
        Stmt::AsmGoto {
            goto_labels,
            outputs,
            inputs,
            ..
        } => {
            for l in goto_labels {
                out.add(&l.name);
            }
            for x in outputs.iter().chain(inputs.iter()) {
                expr_jump_targets(x, out);
            }
        },
        Stmt::Label { stmt, .. } => stmt_jump_targets(stmt, out),
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => expr_jump_targets(e, out),
        Stmt::Return { .. } => {},
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_jump_targets(cond, out);
            stmt_jump_targets(then_, out);
            if let Some(x) = else_ {
                stmt_jump_targets(x, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            expr_jump_targets(cond, out);
            stmt_jump_targets(body, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                stmt_jump_targets(i, out);
            }
            if let Some(c) = cond {
                expr_jump_targets(c, out);
            }
            if let Some(x) = step {
                expr_jump_targets(x, out);
            }
            stmt_jump_targets(body, out);
        },
        Stmt::Block { b, .. } => block_jump_targets(b, out),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            expr_jump_targets(expr, out);
            for it in prelude {
                match it {
                    BlockItem::Stmt(x) => stmt_jump_targets(x, out),
                    BlockItem::Decl(Decl::Func(f)) => {
                        if let Some(body) = &f.body {
                            block_jump_targets(body, out);
                        }
                    },
                    _ => {},
                }
            }
            for c in cases {
                if let Some(v) = &c.val {
                    expr_jump_targets(v, out);
                }
                block_jump_targets(&c.body, out);
            }
        },
        Stmt::Invoke {
            call, landing_pad, ..
        } => {
            // 落地垫是一条由 unwinder 落入的边：它与 `goto` 一样是「非落空入口」。
            if let Some(l) = landing_pad {
                out.add(&l.name);
            }
            stmt_jump_targets(call, out);
        },
        Stmt::EhLandingPad { label, .. } => {
            out.add(&label.name);
        },
        _ => {},
    }
}

/// 本函数有没有**目标集不可知**的跳转（`goto *expr` / `asm goto`）。
///
/// 有 ⇒ A2 在整个函数上不得放宽：间接跳转可以落到任何一个被取址的标签，而「哪些标签
/// 被取址」与「什么时候落进来」在单遍发射里都不可知。宁可不放宽，不可漏一个前驱。
pub fn has_unnamed_jump(f: &FuncDecl) -> bool {
    f.body.as_ref().is_some_and(|b| block_has_unnamed_jump(b))
}

fn block_has_unnamed_jump(b: &Block) -> bool {
    b.items.iter().any(|it| match it {
        BlockItem::Stmt(s) => stmt_has_unnamed_jump(s),
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().is_some_and(block_has_unnamed_jump),
        _ => false,
    })
}

fn stmt_has_unnamed_jump(s: &Stmt) -> bool {
    match s {
        Stmt::GotoIndirect { .. } | Stmt::AsmGoto { .. } => true,
        Stmt::Label { stmt, .. } => stmt_has_unnamed_jump(stmt),
        Stmt::If { then_, else_, .. } => {
            stmt_has_unnamed_jump(then_) || else_.as_ref().is_some_and(|x| stmt_has_unnamed_jump(x))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_has_unnamed_jump(body),
        Stmt::For { init, body, .. } => {
            init.as_ref().is_some_and(|x| stmt_has_unnamed_jump(x)) || stmt_has_unnamed_jump(body)
        },
        Stmt::Block { b, .. } => block_has_unnamed_jump(b),
        Stmt::Switch { cases, prelude, .. } => {
            prelude.iter().any(|it| match it {
                BlockItem::Stmt(s) => stmt_has_unnamed_jump(s),
                _ => false,
            }) || cases.iter().any(|c| block_has_unnamed_jump(&c.body))
        },
        Stmt::Invoke { call, .. } => stmt_has_unnamed_jump(call),
        _ => false,
    }
}

/// 本函数的局部名全集（形参 + 体内任意深度声明的变量，含 `switch` 的 prelude）。
///
/// 为什么要「任意深度」：`addr_escaped_names` 给的也是任意深度的根名，两边口径不一致
/// 就会漏判 —— 只收函数体第一层时，块里 `int v; foo(&v);` 的 `v` 不在 `locals` 里，
/// 交集为空，等于把这条护栏关掉。
fn local_names(f: &FuncDecl) -> HashSet<String> {
    let mut out: HashSet<String> = f
        .params
        .iter()
        .filter_map(|p| p.name.as_ref())
        .map(|i| i.name.clone())
        .collect();
    if let Some(body) = &f.body {
        collect_local_names_block(body, &mut out);
    }
    out
}

fn collect_local_names_block(b: &Block, out: &mut HashSet<String>) {
    for it in &b.items {
        match it {
            BlockItem::Decl(Decl::Var(v)) => {
                out.insert(v.name.name.clone());
            },
            BlockItem::Decl(Decl::Func(f)) => {
                // 嵌套函数的体不属于外层帧：它自己一帧，名字也不与外层相交。
                if let Some(body) = &f.body {
                    collect_local_names_block(body, out);
                }
            },
            BlockItem::Stmt(s) => collect_local_names_stmt(s, out),
            _ => {},
        }
    }
}

fn collect_local_names_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Block { b, .. } => collect_local_names_block(b, out),
        Stmt::If { then_, else_, .. } => {
            collect_local_names_stmt(then_, out);
            if let Some(e) = else_ {
                collect_local_names_stmt(e, out);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::Label { stmt: body, .. } => {
            collect_local_names_stmt(body, out);
        },
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_local_names_stmt(i, out);
            }
            collect_local_names_stmt(body, out);
        },
        Stmt::Switch { cases, prelude, .. } => {
            for it in prelude {
                collect_local_names_item(it, out);
            }
            for c in cases {
                collect_local_names_block(&c.body, out);
            }
        },
        Stmt::Invoke { call, .. } => collect_local_names_stmt(call, out),
        _ => {},
    }
}

fn collect_local_names_item(it: &BlockItem, out: &mut HashSet<String>) {
    match it {
        BlockItem::Stmt(s) => collect_local_names_stmt(s, out),
        BlockItem::Decl(Decl::Var(v)) => {
            out.insert(v.name.name.clone());
        },
        _ => {},
    }
}

/// 本函数有没有 VLA（形参维度或任一深度的局部）。真源与 `opt.rs` 的帧槽分配同源：
/// `Type::has_vla()` 与 `Param::vla_dim`。
pub fn has_vla_local(f: &FuncDecl) -> bool {
    if f.params
        .iter()
        .any(|p| p.vla_dim.is_some() || p.ty.has_vla())
    {
        return true;
    }
    f.body.as_ref().is_some_and(|b| block_has_vla(b))
}

fn block_has_vla(b: &Block) -> bool {
    b.items.iter().any(|it| match it {
        BlockItem::Decl(Decl::Var(v)) => v.ty.has_vla(),
        BlockItem::Decl(Decl::Func(f)) => has_vla_local(f),
        BlockItem::Stmt(s) => stmt_has_vla(s),
        _ => false,
    })
}

fn stmt_has_vla(s: &Stmt) -> bool {
    match s {
        Stmt::Block { b, .. } => block_has_vla(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_vla(then_) || else_.as_ref().is_some_and(|s| stmt_has_vla(s))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::Label { stmt: body, .. } => {
            stmt_has_vla(body)
        },
        Stmt::For { init, body, .. } => {
            init.as_ref().is_some_and(|s| stmt_has_vla(s)) || stmt_has_vla(body)
        },
        Stmt::Switch { cases, prelude, .. } => {
            prelude.iter().any(item_has_vla) || cases.iter().any(|c| block_has_vla(&c.body))
        },
        Stmt::Invoke { call, .. } => stmt_has_vla(call),
        _ => false,
    }
}

fn item_has_vla(it: &BlockItem) -> bool {
    match it {
        BlockItem::Stmt(s) => stmt_has_vla(s),
        BlockItem::Decl(Decl::Var(v)) => v.ty.has_vla(),
        _ => false,
    }
}

/// 体内有没有内联 asm。方案 §4.3 的原文缺陷是「今天只排除『出现在操作数里的名字』」
/// （`regalloc.rs` 的 `asm_names`），而**任何**一处 asm 都可能碰帧内存；本函数回答的是
/// 「这个函数整体要不要关掉事实」，所以只要出现就算命中，不看它碰了哪个名字。
fn block_has_asm(b: &Block) -> bool {
    b.items.iter().any(item_has_asm)
}

fn item_has_asm(it: &BlockItem) -> bool {
    match it {
        BlockItem::Decl(Decl::GlobalAsm { .. }) => true,
        BlockItem::Decl(Decl::Func(f)) => f.body.as_ref().is_some_and(block_has_asm),
        BlockItem::Stmt(s) => stmt_has_asm(s),
        _ => false,
    }
}

fn stmt_has_asm(s: &Stmt) -> bool {
    match s {
        Stmt::AsmGoto { .. } => true,
        Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => expr_has_asm(e),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_has_asm(cond)
                || stmt_has_asm(then_)
                || else_.as_ref().is_some_and(|s| stmt_has_asm(s))
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            expr_has_asm(cond) || stmt_has_asm(body)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_ref().is_some_and(|s| stmt_has_asm(s))
                || cond.as_ref().is_some_and(expr_has_asm)
                || step.as_ref().is_some_and(expr_has_asm)
                || stmt_has_asm(body)
        },
        Stmt::Block { b, .. } => block_has_asm(b),
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            expr_has_asm(expr)
                || prelude.iter().any(item_has_asm)
                || cases
                    .iter()
                    .any(|c| c.val.as_ref().is_some_and(expr_has_asm) || block_has_asm(&c.body))
        },
        Stmt::Invoke { call, .. } => stmt_has_asm(call),
        _ => false,
    }
}

/// 表达式里有没有 asm。**穷尽 `match`**：以后加 `Expr` 变体不填这里就编不过。
fn expr_has_asm(e: &Expr) -> bool {
    use Expr as E;
    match e {
        E::AsmExpr { .. } => true,
        E::Int { .. }
        | E::Float { .. }
        | E::ComplexLit { .. }
        | E::ComplexIntLit { .. }
        | E::String { .. }
        | E::Char { .. }
        | E::Ident { .. }
        | E::SizeofTy { .. }
        | E::AlignofTy { .. }
        | E::LabelAddr { .. }
        | E::VaArgPack { .. } => false,
        E::Real { e, .. }
        | E::Imag { e, .. }
        | E::Unary { e, .. }
        | E::Cast { e, .. }
        | E::SizeofExpr { e, .. }
        | E::AlignofExpr { e, .. }
        | E::AddrOf { e, .. }
        | E::Deref { e, .. }
        | E::PostInc { e, .. }
        | E::PostDec { e, .. }
        | E::PreInc { e, .. }
        | E::PreDec { e, .. }
        | E::Member { expr: e, .. }
        | E::VaArg { ap: e, .. } => expr_has_asm(e),
        E::Binop { l, r, .. }
        | E::Assign { l, r, .. }
        | E::Comma { l, r, .. }
        | E::Index { arr: l, idx: r, .. } => expr_has_asm(l) || expr_has_asm(r),
        E::Cond {
            cond, then_, else_, ..
        } => expr_has_asm(cond) || expr_has_asm(then_) || expr_has_asm(else_),
        E::Call { callee, args, .. } => expr_has_asm(callee) || args.iter().any(expr_has_asm),
        E::CompoundLit { inits, .. } | E::InitList { inits, .. } => inits.iter().any(init_has_asm),
        E::StmtExpr { block, .. } => block_has_asm(block),
        E::Offsetof { indices, .. } => indices.iter().any(|i| expr_has_asm(i)),
    }
}

fn init_has_asm(it: &crate::ast::InitItem) -> bool {
    match it {
        crate::ast::InitItem::Expr(e) => expr_has_asm(e),
        crate::ast::InitItem::Designated { value, .. } => expr_has_asm(value),
    }
}

/// 体内有没有嵌套函数定义（GCC 扩展）：它按外层帧偏移访问外层变量。
fn block_has_nested_fn(b: &Block) -> bool {
    b.items.iter().any(|it| match it {
        BlockItem::Decl(Decl::Func(_)) => true,
        BlockItem::Stmt(s) => match s {
            Stmt::Block { b, .. } => block_has_nested_fn(b),
            Stmt::If { then_, else_, .. } => {
                block_has_nested_fn_stmt(then_)
                    || else_.as_ref().is_some_and(|s| block_has_nested_fn_stmt(s))
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::Label { stmt: body, .. } => {
                block_has_nested_fn_stmt(body)
            },
            Stmt::For { init, body, .. } => {
                init.as_ref().is_some_and(|s| block_has_nested_fn_stmt(s))
                    || block_has_nested_fn_stmt(body)
            },
            Stmt::Switch { cases, prelude, .. } => {
                prelude
                    .iter()
                    .any(|it| matches!(it, BlockItem::Decl(Decl::Func(_))))
                    || cases.iter().any(|c| block_has_nested_fn(&c.body))
            },
            _ => false,
        },
        _ => false,
    })
}

fn block_has_nested_fn_stmt(s: &Stmt) -> bool {
    match s {
        Stmt::Block { b, .. } => block_has_nested_fn(b),
        Stmt::If { then_, else_, .. } => {
            block_has_nested_fn_stmt(then_)
                || else_.as_ref().is_some_and(|s| block_has_nested_fn_stmt(s))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::Label { stmt: body, .. } => {
            block_has_nested_fn_stmt(body)
        },
        Stmt::For { init, body, .. } => {
            init.as_ref().is_some_and(|s| block_has_nested_fn_stmt(s))
                || block_has_nested_fn_stmt(body)
        },
        Stmt::Switch { cases, prelude, .. } => {
            prelude
                .iter()
                .any(|it| matches!(it, BlockItem::Decl(Decl::Func(_))))
                || cases.iter().any(|c| block_has_nested_fn(&c.body))
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 走真解析器拿夹具，而不是手拼 AST。
    ///
    /// 为什么必须走解析器：本模块的每一项判据都是「在真 AST 形状上能不能看到那棵子树」，
    /// 手拼节点只能证明我写了那个臂，证明不了那个臂**收得到**解析器产出的形状
    /// （`Stmt::Switch.prelude` 那一族漏扫就是「判据写在自己想象的形状上」的代价）。
    fn guard_of(src: &str, name: &str) -> FrameGuard {
        let (prog, diags) = crate::parser::parse(src.as_bytes(), "<frame_guard-test>");
        let errs: Vec<String> = diags
            .iter()
            .filter(|d| matches!(d.sev, crate::diagnostics::Severity::Error))
            .map(|d| d.msg.clone())
            .collect();
        assert!(errs.is_empty(), "夹具不该有错：{errs:?}\n源：{src}");
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.name.name == name {
                    return FrameGuard::for_func(f, &HashSet::new());
                }
            }
        }
        panic!("夹具里没有函数 {name}：{src}");
    }

    /// `__builtin_setjmp` 必须被认成二次返回点（方案 §4.1 的那条缺口）。
    #[test]
    fn builtin_setjmp_is_returns_twice() {
        assert!(RETURNS_TWICE_NAMES.contains(&"__builtin_setjmp"));
        assert!(RETURNS_TWICE_NAMES.contains(&"__builtin_longjmp"));
        let g = guard_of("int buf[8]; int g(void) { return __builtin_setjmp(buf); }", "g");
        assert!(g.returns_twice, "__builtin_setjmp 必须置位 returns_twice");
        assert!(!g.allows_caller_saved_home(), "二次返回点所在帧不许绑 caller-saved 家");
        assert!(!g.slots_may_survive_call(), "二次返回点所在帧的槽事实不得跨调用存活");
    }

    /// libc `longjmp` **不得**进这张表：那会把 Lua 整条错误链拉出 SSA（理由见类型注释）。
    #[test]
    fn libc_longjmp_stays_out_of_the_returns_twice_set() {
        for n in ["longjmp", "_longjmp", "siglongjmp"] {
            assert!(
                !RETURNS_TWICE_NAMES.contains(&n),
                "{n} 进影响域会向上拉出整条调用链，Lua 的寄存器家会全丢"
            );
        }
    }

    /// 只有**局部**取址才算 addr_escaped；全局取址不影响本帧。
    #[test]
    fn only_local_address_escape_counts() {
        let global = guard_of("int g; int *p(void) { return &g; }", "p");
        assert!(
            !global.addr_escaped,
            "全局取址不该挡住本帧事实（否则 A1 在任何碰过全局的函数上都是空转）"
        );

        // 深块里的局部取址必须抓到（口径与 addr_escaped_names 一致）。
        let deep = guard_of("void foo(int*); int p(void) { int v; { foo(&v); } return v; }", "p");
        assert!(deep.addr_escaped, "深块里的局部取址必须置位");
        assert!(!deep.slots_may_survive_call(), "取过址的帧槽事实不得跨调用存活");
    }

    /// asm 的口径是「出现即命中」，不看它碰了哪个名字（方案 §4.3）。
    #[test]
    fn any_inline_asm_blocks_the_facts() {
        let g = guard_of(
            "int p(void) { int x = 1; __asm__ volatile(\"\" : \"=r\"(x)); return x; }",
            "p",
        );
        assert!(g.inline_asm, "带输出操作数的 asm 必须置位");
        assert!(!g.slots_may_survive_call());
        assert!(g.blocks_region_home());

        // 不碰任何变量的 `asm("nop")` 同样要拦：它能按任意偏移访存，事实表看不到写集。
        let bare = guard_of("int p(void) { int x = 1; __asm__ volatile(\"nop\"); return x; }", "p");
        assert!(bare.inline_asm, "asm 的口径不看它碰了哪个名字");
    }

    /// 普通函数：四条都必须放行（否则 A1 在任何函数上都是空转）。
    #[test]
    fn plain_function_allows_relaxation() {
        let g = guard_of("int p(int a) { int b = a + 1; return b * 2; }", "p");
        assert_eq!(g, FrameGuard::default());
        assert!(g.slots_may_survive_call());
        assert!(g.allows_caller_saved_home());
        assert!(!g.blocks_region_home());
    }

    /// volatile 与 VLA 两条单独能置位：它们与 addr_escaped / asm 是四个并列条件，
    /// 合并成一条判据就定不出「哪一个漏了」。
    #[test]
    fn volatile_and_vla_each_block_the_facts() {
        let v = guard_of("int p(void) { volatile int x = 1; x = x + 1; return x; }", "p");
        assert!(v.volatile_obj, "volatile 局部必须置位 volatile_obj");
        assert!(v.blocks_region_home(), "volatile（含 pointee）是区域家的硬互斥");

        let vla = guard_of("int p(int n) { int a[n]; a[0] = 1; return a[0]; }", "p");
        assert!(vla.vla, "VLA 必须置位 vla");
        assert!(!vla.slots_may_survive_call());
    }

    /// 嵌套函数：外层变量被嵌套体按帧偏移访问，事实表没有那个视野。
    #[test]
    fn nested_function_blocks_region_home() {
        let g = guard_of("int p(void) { int x = 1; int q(void) { return x; } return q(); }", "p");
        assert!(g.nested_fn, "嵌套函数必须从体里看到");
        assert!(g.blocks_region_home());
    }
}
