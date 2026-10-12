//! src/ast_gvn.rs - AST 风格的局部优化：拷贝传播、GVN/CSE、虚拟寄存器合并。
//!
//! 本模块在 **AST** 上工作（GVN on the AST），与 `crate::ssa::passes::gvn`（在 SSA IR 上做
//! 值编号的那一套）是两套互不相干的实现：这里必须处理「名字后来被改写」的 kill 问题，
//! 那里因为 SSA 名不可变才能安全地跳块复用。
//! 名字里曾有 `ssa_` 前缀，属误导 —— 本模块从不碰 SSA IR —— 已于目录化 Phase 1 去掉。
//!
//! 与 x86_64 栈槽后端配合：消除冗余纯表达式，
//! 合并等价的局部临时变量（类比 register coalescing）。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, Program, Stmt, Type};
use crate::ir::Module;

/// `-O2+`：函数级 GVN + 拷贝传播 + 临时变量合并。
pub fn run(module: &mut Module, level: u8) {
    if level < 2 {
        return;
    }
    // FENV_ACCESS ON: 浮点运算依赖运行时舍入模式/异常标志, 不能跨表达式 CSE.
    let fenv_access = module.source().fenv_access;
    let prog = module.source_mut();
    gvn_program(prog, fenv_access);
}

fn gvn_program(prog: &mut Program, fenv_access: bool) {
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            // Skip GVN for functions with static locals: coalesce_block
            // may alias pointer variables to array-typed variables, and
            // copy propagation can confuse static-local values with their
            // addresses (both share the same Ident key).
            if func_has_static_local(f) {
                continue;
            }
            // 函数级名字事实先算一次再下传: 「这个名字后来有没有被改」只看当前块判不出。
            // 必须在 `&mut f.body` 之前算 —— 两者同时借同一对象会撞borrowck。
            let scope = FnScope::of(f);
            if let Some(body) = &mut f.body {
                gvn_block(body, fenv_access, &scope);
            }
        }
    }
}

fn func_has_static_local(f: &crate::ast::FuncDecl) -> bool {
    f.body.as_ref().is_some_and(block_has_static_local)
}

fn block_has_static_local(b: &crate::ast::Block) -> bool {
    b.items.iter().any(|item| match item {
        crate::ast::BlockItem::Decl(crate::ast::Decl::Var(v)) => v.is_static,
        crate::ast::BlockItem::Stmt(s) => stmt_has_static_local(s),
        _ => false,
    })
}

fn stmt_has_static_local(s: &crate::ast::Stmt) -> bool {
    use crate::ast::Stmt;
    match s {
        Stmt::Block { b, .. } => block_has_static_local(b),
        Stmt::If { then_, else_, .. } => {
            stmt_has_static_local(then_) || else_.as_ref().is_some_and(|e| stmt_has_static_local(e))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_has_static_local(body),
        Stmt::For { body, .. } => stmt_has_static_local(body),
        _ => false,
    }
}

/// 块内值跟踪状态 (可用表达式表 + 拷贝表)。
///
/// 本类型存在的理由是一串静默误编译的同一根因: 旧实现把「表达式值编号 → 已算出它的
/// 变量」当成一张**只增不减**的表, 既不记录某个名字后来被改写, 也不区分「常量」与
/// 「装在变量里的常量」。于是
///
/// ```c
/// int acc = 0;
/// int s = 0;      // 值编号 `i:0` 命中先落表的 acc
/// ```
///
/// 被改写成 `int s = acc;` —— 只要 `acc` 后续被改写, `s` 就拿到错值。实测 `-O2` 下
/// `while (n > 0) { s += acc; acc = acc + n; n--; }` 的结果由 1/5/14 变成 2/6/12。
///
/// AST 层只有**平名字** (没有 SSA 版本), 所以作废判据只能按「读过的名字有没有被写」
/// 来做; 凡是用名字表达不出来的写 (经指针/数组元素/成员的写、调用、内联汇编) 一律
/// 整表作废 —— 粗, 但是这里唯一诚实的判据, 精确版本属于有 use-def 链的 SSA 层 GVN。
#[derive(Clone, Default)]
struct ValTrack {
    /// 表达式键 → (已算出它的变量名, 该表达式读到的名字集)。
    avail: HashMap<String, (String, HashSet<String>)>,
    /// 变量名 → 它被初始化/赋值时的来源名 (拷贝传播)。
    copies: HashMap<String, String>,
}

impl ValTrack {
    /// `written` 里的名字被改写 ⇒ 凡「读到过它」的条目与指向它的拷贝立刻作废;
    /// 以它为目标的那条也作废 (它现在的值已经不是当时那个表达式了)。
    fn kill_names(&mut self, written: &HashSet<String>) {
        if written.is_empty() {
            return;
        }
        self.avail
            .retain(|_, (target, deps)| !written.contains(target) && deps.is_disjoint(written));
        self.copies
            .retain(|tgt, src| !written.contains(tgt) && !written.contains(src));
    }

    /// 名字表达不了的写: 整表作废 (见类型注释)。
    fn kill_all(&mut self) {
        self.avail.clear();
        self.copies.clear();
    }
}

/// 一个表达式里出现的**全部**变量名 (作为值编号的依赖集)。
fn names_in(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Ident { name, .. } => {
            out.insert(name.name.clone());
        },
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            names_in(l, out);
            names_in(r, out);
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. }
        | Expr::Deref { e: inner, .. }
        | Expr::AddrOf { e: inner, .. }
        | Expr::SizeofExpr { e: inner, .. }
        | Expr::AlignofExpr { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => names_in(inner, out),
        Expr::Member { expr: base, .. } => names_in(base, out),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            names_in(cond, out);
            names_in(then_, out);
            names_in(else_, out);
        },
        Expr::Call { callee, args, .. } => {
            names_in(callee, out);
            for a in args {
                names_in(a, out);
            }
        },
        // 其余形态 (字面量、sizeof 类型、offsetof、初始化列表……) 要么不读变量,
        // 要么已由 `is_opaque_expr` 挡在表外, 这里无需再拆。
        _ => {},
    }
}

/// 表达式是否含「读写集合用名字表达不出来」的构造。
fn is_opaque_expr(e: &Expr) -> bool {
    match e {
        Expr::Call { .. } | Expr::AsmExpr { .. } | Expr::VaArg { .. } | Expr::StmtExpr { .. } => {
            true
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            is_opaque_expr(l) || is_opaque_expr(r)
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. }
        | Expr::Deref { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => is_opaque_expr(inner),
        Expr::Cond {
            cond, then_, else_, ..
        } => is_opaque_expr(cond) || is_opaque_expr(then_) || is_opaque_expr(else_),
        _ => false,
    }
}

/// 写入的**根对象名**: `a = …` / `a[i] = …` / `s.x = …` / `++a` 都算写了 `a`。
///
/// 旧实现只认 `Assign { l: Expr::Ident }`, 于是 `a[i] = 0;` 不把 `a` 记为已写,
/// `int b = a[0];` 之后合并就会把 `b` 与 `a` 当成同一个不变的名字。
fn written_root(e: &Expr, out: &mut HashSet<String>) {
    // 与 `collect_expr_writes` 回答的是同一个问题（「这棵表达式改了哪些根对象」),
    // 写两份必然漂移, 故此处只转发。
    collect_expr_writes(e, out);
}

/// 沿 `Index` / `Member` / `Deref` 链找到根对象名; 链上出现 `Deref`/`->` 记为间接写。
fn written_root_chain(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Ident { name, .. } => {
            out.insert(name.name.clone());
        },
        Expr::Index { arr, idx, .. } => {
            written_root_chain(arr, out);
            if is_opaque_expr(idx) {
                out.insert(String::new());
            }
        },
        Expr::Member {
            expr: base,
            is_arrow,
            ..
        } => {
            if *is_arrow {
                out.insert(String::new());
            } else {
                written_root_chain(base, out);
            }
        },
        // `*p = x`: 被写对象没有可记的名字。
        Expr::Deref { .. } => {
            out.insert(String::new());
        },
        _ => {
            out.insert(String::new());
        },
    }
}

/// `out` 中是否出现了「间接写」哨兵 (空串, 见 `written_root_chain`)。
fn has_indirect(out: &HashSet<String>) -> bool {
    out.contains("")
}

/// 函数级名字事实。合并 (coalesce) 的成立条件是「源变量在此后再没被改过」,
/// 而这个判断需要全函数的声明 / 参数 / 取址信息, 只看当前块拿不到。
struct FnScope {
    /// 本函数自己拥有的对象名 (参数 + 任何嵌套块的局部声明)。
    /// **只有它们才能当合并来源**: 文件作用域对象可能被被调者/别的 TU 改写,
    /// 把 `int old = g;` 合并成 `old`→`g` 会让 `old` 变成一个活的别名。
    locals: HashSet<String>,
    /// 被取址的对象名 与 数组类型局部名: 它们可经指针/衰减指针被改写,
    /// 「没有直接赋值」并不等于「不会被改」。
    escaped: HashSet<String>,
    /// **全函数范围**被写过的名字 (含循环 step / cond 里的 `i++`、复合赋值、
    /// 成员/元素写与间接写哨兵)。coalesce 会删掉目标名的声明，所以目标名在
    /// 本块之外还有任何一次写就不能合并 —— 只看当前块的 `assigned` 不够：
    /// `for (int i = ti; i < 5; i++)` 的声明在一个只包含该声明的子块
    /// (for-init) 里，而写 `i` 的 `i++` 属于**外层** `Stmt::For` 节点，
    /// 块内视角完全看不到。当时实测：声明被删后循环读一个从未赋值的 `i`，
    /// `for(int ti=0;ti<5;ti=ti+4){for(int i=ti;i<5;i++)s=s+i;}` 在 -O2 得 9 而应 14。
    assigned: HashSet<String>,
    /// 本函数拥有的每个名字（**参数 + 所有嵌套块的局部声明**）的声明类型。
    ///
    /// 为什么需要它：coalesce 只能在「两边类型完全相同」时把一个名字换成另一个。
    /// 旧实现只从**当前块的声明**里查类型，而参数根本不在其中 —— 查不到就干脆不比，
    /// 于是 `int __seg_gs *lp = p;`（p 是 `int *`）被合并成 `lp`→`p`，pointee 的
    /// `__seg_gs` 连同 `%gs:` 前缀一起消失（实测 -O2 发成普通地址读写）。
    /// 差别的不是“少一次优化”：那是发往错误内存的一次读写。
    types: HashMap<String, Type>,
}

impl FnScope {
    fn of(f: &crate::ast::FuncDecl) -> Self {
        let mut s = FnScope {
            locals: HashSet::new(),
            escaped: HashSet::new(),
            assigned: HashSet::new(),
            types: HashMap::new(),
        };
        for p in &f.params {
            if let Some(n) = &p.name {
                s.locals.insert(n.name.clone());
                s.types.insert(n.name.clone(), p.ty.clone());
            }
        }
        if let Some(body) = &f.body {
            collect_scope(body, &mut s);
        }
        s
    }
}

fn collect_scope(b: &Block, s: &mut FnScope) {
    for item in &b.items {
        // 全函数写入集合: 与本块的内联 `assigned` 分开收集, 因为它要覆盖外层节点
        // (for-step / while-cond / switch 体) 里的写。
        collect_assignments(item, &mut s.assigned, &mut HashMap::new());
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                s.locals.insert(v.name.name.clone());
                s.types.insert(v.name.name.clone(), v.ty.clone());
                // 数组名在表达式里等价于 `&a[0]`, 元素可经该指针被写。
                if matches!(v.ty, Type::Array(_, _)) || is_addr_taken(&v.name.name, b) {
                    s.escaped.insert(v.name.name.clone());
                }
            },
            BlockItem::Stmt(st) => collect_scope_stmt(st, s),
            _ => {},
        }
    }
}

fn collect_scope_stmt(st: &Stmt, s: &mut FnScope) {
    match st {
        Stmt::Block { b, .. } => collect_scope(b, s),
        Stmt::If {
            then_, else_, cond, ..
        } => {
            collect_scope_stmt(then_, s);
            if let Some(e) = else_ {
                collect_scope_stmt(e, s);
            }
            let _ = cond;
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => collect_scope_stmt(body, s),
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_scope_stmt(i, s);
            }
            collect_scope_stmt(body, s);
        },
        Stmt::Label { stmt, .. } => collect_scope_stmt(stmt, s),
        Stmt::Switch { cases, .. } => {
            for c in cases {
                collect_scope(&c.body, s);
            }
        },
        _ => {},
    }
}

/// 名字在本块 (含嵌套块) 内是否被 `&name` 取过址。
fn is_addr_taken(name: &str, b: &Block) -> bool {
    let mut found = false;
    walk_addr_of(b, &mut |root: &str| {
        if root == name {
            found = true;
        }
    });
    found
}

/// 一个块（典型就是一份函数体）内**地址被取走**的对象根名集合。
///
/// 这是「该对象必须留在内存里」这一事实的唯一判据，目前有两个消费者：
///   * GVN 用它决定变量是否参与合并（`is_addr_taken` 的批量版）；
///   * 后端用它决定 `register asm` 绑定能不能生效 —— GCC 对地址外溢的寄存器
///     变量本来就是「放弃绑定、留在内存」，绑定生效才会造成静默旧值。
///
/// 必须走本入口而不是自己爬 AST：逸出判据一旦有两套，其中一套漏报就是错值。
pub fn addr_escaped_names(b: &Block) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    walk_addr_of(b, &mut |root: &str| {
        out.insert(root.to_string());
    });
    out
}

/// 遍历块内所有 `Expr::AddrOf` 的根对象名。
fn walk_addr_of(b: &Block, on_root: &mut dyn FnMut(&str)) {
    for item in &b.items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                if let Some(init) = &v.init {
                    walk_expr_addr_of(init, on_root);
                }
            },
            // 嵌套函数体必须递归进来：`{ int acc; auto void f(void){ g(&acc); } }`
            // 里的 `&acc` 是对**外层函数那个对象**的逸出。以前这条递归缺失，
            // 于是只有后端那份手写递归的版本看得到它，GVN 看不见 —— 同一个
            // 逸出判据两套实现，弱的那一套决定产物（§11.13 的 N8）。
            BlockItem::Decl(Decl::Func(f)) => {
                if let Some(body) = &f.body {
                    walk_addr_of(body, on_root);
                }
            },
            // 块级 asm 也可以是内存约束操作数（把对象地址交出去）。
            BlockItem::Decl(Decl::GlobalAsm {
                outputs,
                output_constraints,
                inputs,
                input_constraints,
                ..
            }) => {
                walk_asm_operands_addr_of(outputs, output_constraints, on_root);
                walk_asm_operands_addr_of(inputs, input_constraints, on_root);
            },
            // 类型定义不携带地址逸出
            BlockItem::Decl(
                Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_) | Decl::Typedef { .. },
            ) => {},
            BlockItem::Stmt(st) => walk_stmt_addr_of(st, on_root),
        }
    }
}

fn walk_stmt_addr_of(st: &Stmt, on_root: &mut dyn FnMut(&str)) {
    match st {
        Stmt::Block { b, .. } => walk_addr_of(b, on_root),
        Stmt::Expr { e, .. } => walk_expr_addr_of(e, on_root),
        Stmt::Return { val, .. } => {
            if let Some(v) = val {
                walk_expr_addr_of(v, on_root);
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            walk_expr_addr_of(cond, on_root);
            walk_stmt_addr_of(then_, on_root);
            if let Some(el) = else_ {
                walk_stmt_addr_of(el, on_root);
            }
        },
        Stmt::While { cond, body, .. } => {
            walk_expr_addr_of(cond, on_root);
            walk_stmt_addr_of(body, on_root);
        },
        Stmt::Do { body, cond, .. } => {
            walk_stmt_addr_of(body, on_root);
            walk_expr_addr_of(cond, on_root);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                walk_stmt_addr_of(i, on_root);
            }
            if let Some(c) = cond {
                walk_expr_addr_of(c, on_root);
            }
            if let Some(sp) = step {
                walk_expr_addr_of(sp, on_root);
            }
            walk_stmt_addr_of(body, on_root);
        },
        Stmt::Switch { expr, cases, .. } => {
            walk_expr_addr_of(expr, on_root);
            for c in cases {
                walk_addr_of(&c.body, on_root);
            }
        },
        Stmt::Label { stmt, .. } => walk_stmt_addr_of(stmt, on_root),
        // `&&label` 跳板只涉及标号，不涉及对象地址。
        Stmt::Goto { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => {},
        Stmt::GotoIndirect { expr, .. } => walk_expr_addr_of(expr, on_root),
        // asm goto 的操作数与普通 asm 同一处理（内存约束 = 把对象地址交出去）。
        Stmt::AsmGoto {
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            ..
        } => {
            walk_asm_operands_addr_of(outputs, output_constraints, on_root);
            walk_asm_operands_addr_of(inputs, input_constraints, on_root);
        },
        Stmt::Invoke { call, .. } => walk_stmt_addr_of(call, on_root),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            walk_expr_addr_of(object, on_root);
            walk_expr_addr_of(selector, on_root);
        },
    }
}

/// 把一组 asm 操作数与其约束串配对遍历：内存约束的操作数根对象算「地址已被取走」。
fn walk_asm_operands_addr_of(ops: &[Expr], constraints: &[String], on_root: &mut dyn FnMut(&str)) {
    for (i, o) in ops.iter().enumerate() {
        if constraints
            .get(i)
            .map(|c| constraint_is_memory(c))
            .unwrap_or(false)
        {
            if let Some(root) = ident_root_name(o) {
                on_root(&root);
            }
        }
        // 操作数本身仍要递归：`"r"(&x)` 里已经有真正的 AddrOf。
        walk_expr_addr_of(o, on_root);
    }
}

/// 该约束是否要求内存操作数（而不是寄存器）。保守判定：只要出现内存类约束字母。
fn constraint_is_memory(c: &str) -> bool {
    c.chars()
        .any(|ch| matches!(ch, 'm' | 'Q' | 'g' | 's' | 'O' | 'v'))
}

fn walk_expr_addr_of(e: &Expr, on_root: &mut dyn FnMut(&str)) {
    match e {
        Expr::AddrOf { e: inner, .. } => {
            if let Some(root) = ident_root_name(inner) {
                on_root(&root);
            }
            walk_expr_addr_of(inner, on_root);
        },
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            walk_expr_addr_of(l, on_root);
            walk_expr_addr_of(r, on_root);
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. }
        | Expr::Deref { e: inner, .. }
        | Expr::SizeofExpr { e: inner, .. }
        | Expr::AlignofExpr { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => walk_expr_addr_of(inner, on_root),
        Expr::Member { expr: base, .. } => walk_expr_addr_of(base, on_root),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            walk_expr_addr_of(cond, on_root);
            walk_expr_addr_of(then_, on_root);
            walk_expr_addr_of(else_, on_root);
        },
        Expr::Call { callee, args, .. } => {
            walk_expr_addr_of(callee, on_root);
            for a in args {
                walk_expr_addr_of(a, on_root);
            }
        },
        // 以下四个变体以前全部落在 `_ => {}` 里，也就是**静默不递归**。它们不是
        // 罕见形态：`({ int *p = &x; })` 里的 `&x` 被漏报，GVN 就会把 `x` 当成
        // 「没被取过址」而参与合并；同一个判据将来也要管住寄存器绑定。
        // 这里不留 `_`：以后新增 `Expr` 变体会直接编不过，而不是静默漏报。
        Expr::StmtExpr { block, .. } => walk_addr_of(block, on_root),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    crate::ast::InitItem::Expr(e) => walk_expr_addr_of(e, on_root),
                    crate::ast::InitItem::Designated { value, .. } => {
                        walk_expr_addr_of(value, on_root)
                    },
                }
            }
        },
        Expr::AsmExpr {
            outputs,
            output_constraints,
            inputs,
            input_constraints,
            ..
        } => {
            walk_asm_operands_addr_of(outputs, output_constraints, on_root);
            walk_asm_operands_addr_of(inputs, input_constraints, on_root);
        },
        // `va_arg(ap, T)` 会原地修改 va_list 对象，它的地址必然对外可见。
        Expr::VaArg { ap, .. } => {
            if let Some(root) = ident_root_name(ap) {
                on_root(&root);
            }
            walk_expr_addr_of(ap, on_root);
        },
        // 叶子：没有子表达式（`LabelAddr` 是 `&&label`，只涉及标号）。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::Ident { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::Offsetof { .. }
        | Expr::LabelAddr { .. } => {},
    }
}

/// 表达式链上的根 Ident 名 (`a[i].x` → `a`); 链上出现 `Deref`/`->` 则无具名根。
fn ident_root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(name.name.clone()),
        Expr::Index { arr, .. } => ident_root_name(arr),
        Expr::Member {
            expr: base,
            is_arrow,
            ..
        } if !is_arrow => ident_root_name(base),
        Expr::Cast { e: inner, .. } => ident_root_name(inner),
        _ => None,
    }
}

fn gvn_block(block: &mut Block, fenv_access: bool, scope: &FnScope) {
    let mut tr = ValTrack::default();
    for item in &mut block.items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                // 声明本身就是一次写入: 先把旧同名条目作废 (嵌套块里的同名重声明),
                // 再考虑把初值入表。
                tr.kill_names(&[v.name.name.clone()].into_iter().collect());
                if let Some(init) = &mut v.init {
                    let name = v.name.name.clone();
                    propagate_copies_expr(init, &tr.copies);
                    let opaque = is_opaque_expr(init);
                    let mut deps = HashSet::new();
                    names_in(init, &mut deps);
                    if let Some(key) = gvn_key_string(init, fenv_access) {
                        if let Some((prev, _)) = tr.avail.get(&key) {
                            let prev = prev.clone();
                            *init = ident_expr(&prev, init.ty());
                            tr.copies.insert(name, prev);
                        } else if !opaque && !deps.contains(&name) && !key_is_bare_constant(&key) {
                            // `x = x + 1` 形态不得入表: 平名字下「旧 x + 1」与「新 x」
                            // 无法区分, 入表就等于给后面一条错误的替换。
                            tr.avail.insert(key, (name, deps));
                        }
                    }
                    if opaque {
                        tr.kill_all();
                    }
                }
            },
            BlockItem::Stmt(s) => gvn_stmt(s, &mut tr, fenv_access, scope),
            _ => {},
        }
    }
    coalesce_block(block, scope);
}

/// 语句内所有可外推的拷贝来源替换 + 值编号替换, 同时维持 `ValTrack` 的作废。
fn gvn_stmt(s: &mut Stmt, tr: &mut ValTrack, fenv_access: bool, scope: &FnScope) {
    match s {
        Stmt::Return { val: Some(e), .. } => {
            propagate_copies_expr(e, &tr.copies);
            if is_opaque_expr(e) {
                tr.kill_all();
            }
        },
        Stmt::Expr { e, .. } => {
            if let Expr::Assign { l, r, .. } = e {
                let mut deps = HashSet::new();
                names_in(&**r, &mut deps);
                propagate_copies_expr(&mut **r, &mut tr.copies);
                if let Expr::Ident { name, .. } = &**l {
                    let target = name.name.clone();
                    // 调用/汇编/语句表达式, 或右值里出现过间接写 ⇒ 名字表不可信。
                    let mut rhs_writes = HashSet::new();
                    collect_expr_writes(&**r, &mut rhs_writes);
                    let opaque = is_opaque_expr(&**r) || has_indirect(&rhs_writes);
                    if let Some(key) = gvn_key_string(&**r, fenv_access) {
                        if let Some((prev, _)) = tr.avail.get(&key) {
                            let prev = prev.clone();
                            **r = ident_expr(&prev, r.ty());
                            tr.copies.insert(target.clone(), prev);
                        } else if !opaque && !deps.contains(&target) && !key_is_bare_constant(&key)
                        {
                            tr.avail.insert(key, (target.clone(), deps.clone()));
                        }
                    }
                    // 写入目标后, 任何读过它的条目立刻作废 (顺序: 先入表再杀目标旧值,
                    // 本条自己的新条目依赖集不含 target, 上面已经拦过)。
                    tr.kill_names(&[target].into_iter().collect());
                    if opaque {
                        tr.kill_all();
                    }
                } else {
                    // `*p = x` / `a[i] = x` / `s.x = y`: 被写对象名字不可知。
                    propagate_copies_expr(e, &mut tr.copies);
                    tr.kill_all();
                }
            } else {
                propagate_copies_expr(e, &mut tr.copies);
                let mut written = HashSet::new();
                written_root(e, &mut written);
                if has_indirect(&written) || is_opaque_expr(e) {
                    tr.kill_all();
                } else {
                    tr.kill_names(&written);
                }
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            propagate_copies_expr(cond, &mut tr.copies);
            let mut cond_writes = HashSet::new();
            written_root(cond, &mut cond_writes);
            if has_indirect(&cond_writes) || is_opaque_expr(cond) {
                tr.kill_all();
            } else {
                tr.kill_names(&cond_writes);
            }
            // 分支里可能改写任何名字: 分支内用副本跑, 结果不外泄, 汇合点整表作废。
            let mut sub = tr.clone();
            gvn_stmt(then_, &mut sub, fenv_access, scope);
            if let Some(el) = else_ {
                let mut sub2 = tr.clone();
                gvn_stmt(el, &mut sub2, fenv_access, scope);
            }
            tr.kill_all();
        },
        Stmt::Block { b, .. } => {
            gvn_block(b, fenv_access, scope);
            // 块内的声明与写入对外可见 (外层同名对象可被改写), 表必须作废。
            tr.kill_all();
        },
        Stmt::While { cond, body, .. } => {
            propagate_copies_expr(cond, &mut tr.copies);
            let mut cw = HashSet::new();
            written_root(cond, &mut cw);
            if has_indirect(&cw) || is_opaque_expr(cond) {
                tr.kill_all();
            } else {
                tr.kill_names(&cw);
            }
            let mut sub = ValTrack::default();
            gvn_stmt(body, &mut sub, fenv_access, scope);
            tr.kill_all();
        },
        Stmt::Do { body, cond, .. } => {
            let mut sub = ValTrack::default();
            gvn_stmt(body, &mut sub, fenv_access, scope);
            tr.kill_all();
            propagate_copies_expr(cond, &mut tr.copies);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                gvn_stmt(i, tr, fenv_access, scope);
            }
            if let Some(c) = cond {
                propagate_copies_expr(c, &mut tr.copies);
            }
            if let Some(st) = step {
                propagate_copies_expr(st, &mut tr.copies);
            }
            // 循环体每轮重求值, 携带值使外层表不可信: 体内用全新状态跑。
            let mut sub = ValTrack::default();
            gvn_stmt(body, &mut sub, fenv_access, scope);
            tr.kill_all();
        },
        _ => {},
    }
}

/// 合并「仅拷贝另一局部变量」且后来没被改过的冗余 decl（虚拟寄存器 coalesce）。
///
/// 成立条件比旧实现写的「不在 assigned 里」严得多, 因为旧 `assigned` 只认
/// `Assign { l: Expr::Ident }` 一种形态, 于是漏了三条改写路径:
///   * `a[i] = x;` / `s.f = x;` 不把根对象 `a`/`s` 记为已写;
///   * `int *p = &acc;` 之后的 `*p = 5;` 改的是 `acc`, 而 `acc` 从未进过 `assigned`;
///   * 来源名可以是**文件作用域对象**, 被调者随时能改它。
/// 三者都会把 `int old = acc;` 合并成 `old`→`acc` 的**活别名** (后续改 `acc` 时 `old`
/// 跟着变), 即与源语义不符的静默错值。本版的判据: 双方都必须是本函数拥有的局部/参数、
/// 都没有被直接赋值 (按根对象名计、含间接写哨兵)、都没有被取址且不是数组。
fn coalesce_block(block: &mut Block, scope: &FnScope) {
    let mut alias: HashMap<String, String> = HashMap::new();
    let mut assigned: HashSet<String> = HashSet::new();
    let mut decl_counts: HashMap<String, u32> = HashMap::new();
    for item in &block.items {
        collect_assignments(item, &mut assigned, &mut decl_counts);
    }
    // 类型比对用的表在 `scope.types`（参数 + 全函数嵌套声明）。旧实现只收本块的
    // Decl，参数查不到就整段跳过比对 —— 那是下面「类型未知 ⇒ 不合并」这条规则
    // 要堵的洞（见 `FnScope::types` 的注释）。
    // 间接写 (`*p = x` / `p->f = x`) 改的是哪个名字 AST 层判不了, 只能把所有被取址
    // 与数组类型的局部一律视为已写。
    let opaque_write = assigned.contains("") || scope.assigned.contains("");
    let stable_copy_source = |n: &str, is_dst: bool| -> bool {
        if !scope.locals.contains(n) || scope.escaped.contains(n) {
            return false;
        }
        if assigned.contains(n) {
            return false;
        }
        // 全函数视角: 目标名在本块之外 (外层循环 step/cond、兄弟块、switch 体)
        // 还被写过 ⇒ 它不是一次性拷贝; 而 dst 的声明会被删, 删错就是悬空局部。
        if scope.assigned.contains(n) {
            return false;
        }
        if opaque_write {
            return false;
        }
        // 目标名在本块被重声明过 ⇒ 它不是「一次性拷贝」。
        !is_dst || decl_counts.get(n).copied().unwrap_or(0) <= 1
    };
    for item in &block.items {
        if let BlockItem::Decl(Decl::Var(v)) = item {
            if let Some(Expr::Ident { name, .. }) = &v.init {
                let dst = v.name.name.as_str();
                let src = name.name.as_str();
                if dst == src {
                    continue;
                }
                if stable_copy_source(dst, true) && stable_copy_source(src, false) {
                    // Don't alias pointer to array/struct/union: array-to-pointer
                    // decay computes address at each use site, but aliasing would
                    // make codegen treat the array name as a pre-computed pointer,
                    // skipping the LEA instruction.
                    // 别名只在**类型完全相同**时成立。旧判据用的是 `mem::discriminant`,
                    // 那只比枚举变体：`int __seg_gs *` 与 `int *` 都是 `Type::Ptr(..)`，
                    // 于是把带段的拷贝目标换成不带段的源名字 —— **pointee 的 QualType
                    // （`__seg_gs` / `const` / `volatile`）跟着消失**。实测后果不是“少一次
                    // 优化”而是错值：-O2 下 `int __seg_gs *lp = p; return *lp;` 发成普通
                    // 地址读写（-O0/-O1 正常），在用户态是 GS base 为 0 所以看不出来，
                    // 在内核 per-cpu 场景就是往错误的内存读写。
                    let src_ty = scope.types.get(src);
                    let dst_ty = scope.types.get(dst);
                    let (Some(s), Some(d)) = (src_ty, dst_ty) else {
                        // 类型未知 ⇒ 不合并。宁可少一次优化，也不能拿“看不到类型”
                        // 当“类型相同”。
                        continue;
                    };
                    if s != d {
                        continue;
                    }
                    alias.insert(dst.to_string(), src.to_string());
                }
            }
        }
    }
    if alias.is_empty() {
        return;
    }
    // 原子性: 先在副本上改写, 一旦遇到本重写器覆盖不到的形态就整批丢弃。
    // 「半途改名 + 删声明」就是本函数过去的失效形态: 名字留在原地而它的声明被删,
    // 后端只能把它当外部符号 (`h(3,4)` 编成 `call h` → `undefined reference to 'h'`)。
    let mut trial = block.clone();
    if !rewrite_block_aliases(&mut trial, &alias) {
        return;
    }
    *block = trial;
    block.items.retain(
        |item| !matches!(item, BlockItem::Decl(Decl::Var(v)) if alias.contains_key(&v.name.name)),
    );
}

/// 在块上应用别名改写; 返回 `false` = 遇到覆盖不到的形态。
fn rewrite_block_aliases(b: &mut Block, alias: &HashMap<String, String>) -> bool {
    b.items
        .iter_mut()
        .all(|item| rewrite_item_aliases(item, alias))
}

fn rewrite_item_aliases(item: &mut BlockItem, alias: &HashMap<String, String>) -> bool {
    match item {
        BlockItem::Decl(Decl::Var(v)) => v
            .init
            .as_mut()
            .map_or(true, |init| rewrite_expr_aliases(init, alias)),
        BlockItem::Stmt(s) => rewrite_stmt_aliases(s, alias),
        // 嵌套函数定义有自己的作用域, 其体内可能出现被合并的名字 ⇒ 保守放弃。
        BlockItem::Decl(_) => false,
    }
}

fn collect_assignments(
    item: &BlockItem,
    assigned: &mut HashSet<String>,
    decl_counts: &mut HashMap<String, u32>,
) {
    match item {
        BlockItem::Decl(Decl::Var(v)) => {
            *decl_counts.entry(v.name.name.clone()).or_insert(0) += 1;
            if let Some(init) = &v.init {
                collect_expr_writes(init, assigned);
            }
        },
        BlockItem::Stmt(s) => collect_assignments_stmt(s, assigned, decl_counts),
        _ => {},
    }
}

/// 表达式树里所有「写」的根对象名; 间接写记为空串哨兵。
fn collect_expr_writes(e: &Expr, assigned: &mut HashSet<String>) {
    match e {
        Expr::Assign { l, r, .. } => {
            written_root_chain(l, assigned);
            collect_expr_writes(r, assigned);
        },
        Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => {
            written_root_chain(inner, assigned);
            collect_expr_writes(inner, assigned);
        },
        Expr::Binop { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            collect_expr_writes(l, assigned);
            collect_expr_writes(r, assigned);
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. }
        | Expr::Deref { e: inner, .. }
        | Expr::AddrOf { e: inner, .. }
        | Expr::SizeofExpr { e: inner, .. }
        | Expr::AlignofExpr { e: inner, .. } => collect_expr_writes(inner, assigned),
        Expr::Member { expr: base, .. } => collect_expr_writes(base, assigned),
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            collect_expr_writes(cond, assigned);
            collect_expr_writes(then_, assigned);
            collect_expr_writes(else_, assigned);
        },
        Expr::Call { callee, args, .. } => {
            collect_expr_writes(callee, assigned);
            for a in args {
                collect_expr_writes(a, assigned);
            }
        },
        _ => {},
    }
}

fn collect_assignments_stmt(
    s: &Stmt,
    assigned: &mut HashSet<String>,
    decl_counts: &mut HashMap<String, u32>,
) {
    match s {
        Stmt::Expr { e, .. } => collect_expr_writes(e, assigned),
        Stmt::Return { val: Some(e), .. } => collect_expr_writes(e, assigned),
        Stmt::Block { b, .. } => {
            for sub in &b.items {
                collect_assignments(sub, assigned, decl_counts);
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            collect_expr_writes(cond, assigned);
            collect_assignments_stmt(then_, assigned, decl_counts);
            if let Some(e) = else_ {
                collect_assignments_stmt(e, assigned, decl_counts);
            }
        },
        Stmt::While { cond, body, .. } => {
            collect_expr_writes(cond, assigned);
            collect_assignments_stmt(body, assigned, decl_counts);
        },
        Stmt::Do { body, cond, .. } => {
            collect_assignments_stmt(body, assigned, decl_counts);
            collect_expr_writes(cond, assigned);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_assignments_stmt(i, assigned, decl_counts);
            }
            if let Some(c) = cond {
                collect_expr_writes(c, assigned);
            }
            if let Some(st) = step {
                collect_expr_writes(st, assigned);
            }
            collect_assignments_stmt(body, assigned, decl_counts);
        },
        Stmt::Label { stmt, .. } => collect_assignments_stmt(stmt, assigned, decl_counts),
        Stmt::Switch { expr, cases, .. } => {
            collect_expr_writes(expr, assigned);
            for c in cases {
                for sub in &c.body.items {
                    collect_assignments(sub, assigned, decl_counts);
                }
            }
        },
        _ => {},
    }
}

/// 语句层的别名改写。返回 `false` = 该形态本重写器覆盖不到, 调用方必须丢弃整批改写。
///
/// 为什么必须把「覆盖不到」显式上报而不是 `_ => {}` 静默跳过: 跳过一个含变量引用的
/// 子树, 等于让那个名字继续用旧名, 而它的声明已被本次合并删掉 —— 未声明的名字在
/// 后端只能按外部符号处理。旧实现完全没有 `Expr::Call` 分支, 于是
/// `int (*h)(int,int) = add;` 被合并后 `h(3,4)` 里的 `h` 没改名 → `undefined reference
/// to 'h'` (带同名全局符号时则静默错跳)。
fn rewrite_stmt_aliases(s: &mut Stmt, alias: &HashMap<String, String>) -> bool {
    match s {
        Stmt::Expr { e, .. } => rewrite_expr_aliases(e, alias),
        Stmt::Return { val, .. } => val
            .as_mut()
            .map_or(true, |e| rewrite_expr_aliases(e, alias)),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            rewrite_expr_aliases(cond, alias)
                && rewrite_stmt_aliases(then_, alias)
                && else_
                    .as_mut()
                    .map_or(true, |e| rewrite_stmt_aliases(e, alias))
        },
        Stmt::Block { b, .. } => rewrite_block_aliases(b, alias),
        Stmt::While { cond, body, .. } => {
            rewrite_expr_aliases(cond, alias) && rewrite_stmt_aliases(body, alias)
        },
        Stmt::Do { body, cond, .. } => {
            rewrite_stmt_aliases(body, alias) && rewrite_expr_aliases(cond, alias)
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            init.as_mut()
                .map_or(true, |i| rewrite_stmt_aliases(i, alias))
                && cond
                    .as_mut()
                    .map_or(true, |c| rewrite_expr_aliases(c, alias))
                && step
                    .as_mut()
                    .map_or(true, |st| rewrite_expr_aliases(st, alias))
                && rewrite_stmt_aliases(body, alias)
        },
        // 标签名与变量名不在同一命名空间, 只递归其内嵌语句。
        Stmt::Label { stmt, .. } => rewrite_stmt_aliases(stmt, alias),
        Stmt::Switch { expr, cases, .. } => {
            rewrite_expr_aliases(expr, alias)
                && cases.iter_mut().all(|c| {
                    c.val
                        .as_mut()
                        .map_or(true, |v| rewrite_expr_aliases(v, alias))
                        && c.range_end
                            .as_mut()
                            .map_or(true, |v| rewrite_expr_aliases(v, alias))
                        && rewrite_block_aliases(&mut c.body, alias)
                })
        },
        Stmt::GotoIndirect { expr, .. } => rewrite_expr_aliases(expr, alias),
        // 不含变量引用的控制流形态。
        Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. }
        | Stmt::Goto { .. } => true,
        // asm 的操作数按字符串约定、EH 交接与落地垫的名字语义特殊 ⇒ 不改写。
        Stmt::AsmGoto { .. } | Stmt::Invoke { .. } | Stmt::EhLandingPad { .. } => false,
    }
}

fn rewrite_expr_aliases(e: &mut Expr, alias: &HashMap<String, String>) -> bool {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(target) = alias.get(&name.name) {
                name.name = target.clone();
            }
            true
        },
        Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } | Expr::Comma { l, r, .. } => {
            rewrite_expr_aliases(l, alias) && rewrite_expr_aliases(r, alias)
        },
        Expr::Unary { e: inner, .. }
        | Expr::Cast { e: inner, .. }
        | Expr::Real { e: inner, .. }
        | Expr::Imag { e: inner, .. }
        | Expr::Deref { e: inner, .. }
        | Expr::AddrOf { e: inner, .. }
        | Expr::SizeofExpr { e: inner, .. }
        | Expr::AlignofExpr { e: inner, .. }
        | Expr::PostInc { e: inner, .. }
        | Expr::PostDec { e: inner, .. }
        | Expr::PreInc { e: inner, .. }
        | Expr::PreDec { e: inner, .. } => rewrite_expr_aliases(inner, alias),
        Expr::Member { expr: base, .. } => rewrite_expr_aliases(base, alias),
        Expr::Index { arr, idx, .. } => {
            rewrite_expr_aliases(arr, alias) && rewrite_expr_aliases(idx, alias)
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            rewrite_expr_aliases(cond, alias)
                && rewrite_expr_aliases(then_, alias)
                && rewrite_expr_aliases(else_, alias)
        },
        // 关键补漏: 经函数指针变量的调用 —— 被调者与每个实参都可能是被合并的名字。
        Expr::Call { callee, args, .. } => {
            rewrite_expr_aliases(callee, alias)
                && args.iter_mut().all(|a| rewrite_expr_aliases(a, alias))
        },
        Expr::Offsetof { indices, .. } => {
            indices.iter_mut().all(|i| rewrite_expr_aliases(i, alias))
        },
        // 字面量形态不引用变量。
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. } => true,
        // 其余形态 (asm 表达式 / 复合字面量与指定初始化 / 语句表达式 / 变参 /
        // 标签地址 / 将来新增的形态) 一律当覆盖不到 ⇒ 整批改写作废。
        _ => false,
    }
}

fn propagate_copies_expr(e: &mut Expr, copies: &HashMap<String, String>) {
    match e {
        Expr::Ident { name, .. } => {
            if let Some(src) = copies.get(&name.name) {
                name.name = src.clone();
            }
        },
        Expr::Binop { l, r, .. } => {
            propagate_copies_expr(l, copies);
            propagate_copies_expr(r, copies);
        },
        Expr::Unary { e: inner, .. } => propagate_copies_expr(inner, copies),
        Expr::Assign { l, r, .. } => {
            propagate_copies_expr(l, copies);
            propagate_copies_expr(r, copies);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            propagate_copies_expr(cond, copies);
            propagate_copies_expr(then_, copies);
            propagate_copies_expr(else_, copies);
        },
        Expr::Cast { e: inner, .. } => propagate_copies_expr(inner, copies),
        Expr::Index { arr, idx, .. } => {
            propagate_copies_expr(arr, copies);
            propagate_copies_expr(idx, copies);
        },
        _ => {},
    }
}

/// 值编号的键。**必须携带结果类型**: 旧键只记运算与操作数, 于是
/// `long b = x + y;` 与 `int a = x + y;` 得到同一个键, `b` 被替换成 `a` 后按 `int`
/// 记账, 高于 32 位的结果直接截断。类型串由 `Debug` 输出 (对同一 `Type` 确定),
/// 不引入新的哈希轴。
fn gvn_key_string(e: &Expr, fenv_access: bool) -> Option<String> {
    // Volatile expressions must not be CSE'd
    if crate::ast::type_has_volatile(&e.ty()) {
        return None;
    }
    let inner = gvn_key_inner(e, fenv_access)?;
    Some(format!("{}#{}", inner, type_tag(&e.ty())))
}

/// 类型标签: 只需「同类型同标签、不同类型不同标签」。
fn type_tag(ty: &Type) -> String {
    format!("{:?}", ty)
}

/// 纯常量键 (`i:<值>`) 不能作为「已算出它的变量」入表: 常量本身已经是
/// 最廉价的形式, 把它换成一个装在变量里的常量既无收益, 又多一条依赖内存生命期的
/// 正确性负担 (`int acc = 0; int s = 0;` 被改写成 `int s = acc;`)。运算内部的常量仍
/// 照常参与编号 (`x + 1` 这类复合键可以入表)。
fn key_is_bare_constant(key: &str) -> bool {
    key.starts_with("i:")
}

fn gvn_key_inner(e: &Expr, fenv_access: bool) -> Option<String> {
    match e {
        Expr::Int { v, .. } => Some(format!("i:{v}")),
        Expr::Ident { name, .. } => Some(format!("v:{}", name.name)),
        // FENV_ACCESS ON 时浮点运算不得 CSE (舍入模式/异常标志可能因环境
        // 修改而不同); 整数纯运算仍可参与值编号.
        Expr::Binop { op, l, r, ty, .. }
            if is_gvn_pure_op(*op)
                && (!fenv_access
                    || !matches!(ty, Type::Float | Type::Double | Type::LongDouble)) =>
        {
            Some(format!(
                "b:{op:?}:{}:{}",
                gvn_key_string(l, fenv_access)?,
                gvn_key_string(r, fenv_access)?
            ))
        },
        Expr::Unary { op, e: inner, .. } => {
            Some(format!("u:{op:?}:{}", gvn_key_string(inner, fenv_access)?))
        },
        _ => None,
    }
}

fn is_gvn_pure_op(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::And
            | BinOp::Or
            | BinOp::Xor
            | BinOp::Shl
            | BinOp::Shr
    )
}

fn ident_expr(name: &str, ty: Type) -> Expr {
    use crate::diagnostics::Span;
    Expr::Ident {
        name: crate::ast::Ident {
            name: name.to_string(),
            span: Span::point("gvn", 1, 1),
        },
        span: Span::point("gvn", 1, 1),
        ty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{FuncDecl, Ident, Type};
    use crate::diagnostics::Span;
    use crate::ir;

    fn int_ty() -> Type {
        Type::Int
    }

    #[test]
    fn gvn_reuses_prior_add() {
        let sp = Span::point("t.c", 1, 1);
        let x = || Expr::Ident {
            name: Ident {
                name: "x".into(),
                span: sp.clone(),
            },
            span: sp.clone(),
            ty: int_ty(),
        };
        let y = || Expr::Ident {
            name: Ident {
                name: "y".into(),
                span: sp.clone(),
            },
            span: sp.clone(),
            ty: int_ty(),
        };
        let add = || Expr::Binop {
            op: BinOp::Add,
            l: Box::new(x()),
            r: Box::new(y()),
            span: sp.clone(),
            ty: int_ty(),
        };
        let prog = Program {
            decls: vec![Decl::Func(FuncDecl {
                ret_ty: int_ty(),
                name: Ident {
                    name: "f".into(),
                    span: sp.clone(),
                },
                params: vec![],
                body: Some(Block {
                    items: vec![
                        BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                            ty: int_ty(),
                            name: Ident {
                                name: "a".into(),
                                span: sp.clone(),
                            },
                            init: Some(add()),
                            span: sp.clone(),
                            is_extern: false,
                            is_static: false,
                            section: None,
                            alignment: None,
                            is_weak: false,
                            visibility: None,
                            is_unused: false,
                            register_name: None,
                            home: None,
                            cleanup_fn: None,
                            is_thread_local: false,
                            no_sanitize: vec![],
                            alias_name: None,
                            copy_fn: None,
                            is_retain: false,
                            is_used_attr: false,
                            quals: crate::ast::QualifierFlags::NONE,
                            addr_space: crate::ast::AddrSpace::None,
                            tls_model: None,
                            asm_label: None,
                        })),
                        BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                            ty: int_ty(),
                            name: Ident {
                                name: "b".into(),
                                span: sp.clone(),
                            },
                            init: Some(add()),
                            span: sp.clone(),
                            is_extern: false,
                            is_static: false,
                            section: None,
                            alignment: None,
                            is_weak: false,
                            visibility: None,
                            is_unused: false,
                            register_name: None,
                            home: None,
                            cleanup_fn: None,
                            is_thread_local: false,
                            no_sanitize: vec![],
                            alias_name: None,
                            copy_fn: None,
                            is_retain: false,
                            is_used_attr: false,
                            quals: crate::ast::QualifierFlags::NONE,
                            addr_space: crate::ast::AddrSpace::None,
                            tls_model: None,
                            asm_label: None,
                        })),
                    ],
                    span: sp.clone(),
                }),
                span: sp.clone(),
                is_static: false,
                is_extern: false,
                is_variadic: false,
                is_weak: false,
                is_constructor: false,
                is_destructor: false,
                is_nested: false,
                is_cold: false,
                is_hot: false,
                section: None,
                alignment: None,
                visibility: None,
                alias_name: None,
                upvars: vec![],
                is_unused: false,
                is_noinline: false,
                is_always_inline: false,
                is_inline: false,
                is_old_style: false,
                format_kind: None,
                format_string_idx: None,
                format_first_arg: None,
                no_sanitize: vec![],
                target_clones: vec![],
                returns_nonnull: false,
                ctor_priority: None,
                dtor_priority: None,
                is_retain: false,
                is_used_attr: false,
                is_flatten: false,
                error_msg: None,
                warning_msg: None,
                deprecated_msg: None,
                ifunc_resolver: None,
                asm_output_template: None,
                is_global: false,
                is_device: false,
                is_host: false,
                is_naked: false,
                is_noreturn: false,
                is_pure: false,
                is_const_attr: false,
                returns_twice: false,
                no_instrument: false,
                is_malloc: false,
                malloc_dealloc: None,
                malloc_dealloc_argno: None,
                alloc_size: None,
                assume_aligned: None,
                warn_unused_result: false,
                is_sentinel: false,
                format_arg_idx: None,
                is_nodiscard: false,
                asm_label: None,
            })],
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
        };
        let (mut module, _) = ir::lower(prog).expect("lower");
        run(&mut module, 2);
        let body = match &module.source().decls[0] {
            Decl::Func(f) => f.body.as_ref().unwrap(),
            _ => panic!("func"),
        };
        assert_eq!(body.items.len(), 1, "coalesce should drop redundant b decl");
        if let BlockItem::Decl(Decl::Var(v)) = &body.items[0] {
            assert_eq!(v.name.name, "a");
        }
    }

    /// 取一段源码里某个函数的「地址逸出」集。
    fn escapes(src: &str, fname: &str) -> Vec<String> {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        for decl in prog.decls.iter() {
            if let Decl::Func(f) = decl {
                if f.name.name == fname {
                    let b = f.body.as_ref().expect("body");
                    let mut v: Vec<String> = addr_escaped_names(b).into_iter().collect();
                    v.sort();
                    return v;
                }
            }
        }
        panic!("找不到函数 {fname}");
    }

    /// 逸出收集必须覆盖每一种可以携带 `&x` 的表达式形态。
    ///
    /// 语句表达式、复合字面量/初始化列表、内联汇编操作数、va_arg 这几个变体以前
    /// 全部落在 walker 的 `_ => {}` 里**静默不递归**：GVN 因此把被取址的变量当成
    /// 可合并，后端也因此给它留下 `register asm` 绑定（两者都是静默错值）。
    /// 反向对照（不取址时必须为空）同样必要：否则「一律认为逸出」也能让上面全绿。
    #[test]
    fn addr_escaped_names_covers_every_escape_shape() {
        assert_eq!(escapes("int f(void){ int x; return (int)(long)&x; }\n", "f"), ["x"]);
        assert_eq!(
            escapes("int f(void){ int a[4]; return (int)(long)&a[1]; }\n", "f"),
            ["a"],
            "下标的根对象才是逸出的那个"
        );
        assert_eq!(
            escapes("struct S{int g;};\nint f(void){ struct S s; return (int)(long)&s.g; }\n", "f"),
            ["s"]
        );
        assert_eq!(
            escapes("int f(void){ int x; ({ int *p = &x; *p; }); return 0; }\n", "f"),
            ["x"],
            "语句表达式里的 &x 不得漏报"
        );
        assert_eq!(
            escapes("int f(int v){ int x; __asm__ volatile(\"\" : : \"m\"(x)); return v; }\n", "f"),
            ["x"],
            "asm 内存约束操作数 = 把对象地址交出去"
        );
        assert_eq!(
            escapes(
                "int f(void){ int x; int *q = (int *[]){ &x }[0]; return (int)(long)q; }\n",
                "f"
            ),
            ["x"],
            "复合字面量的初始化式里也得递归"
        );
        // N8：嵌套函数体里的 `&acc` 也是对外层对象的逸出。后端以前为了这件事自己手写
        // 一份递归，于是同一个事实有两套判据，而弱的那一套决定产物（GVN 看不见就把 `acc`
        // 当可合并的临时值）。现在递归住在判据本身里，后端只递一次入口 —— 这一行就是
        // 那次「删副本不留缺口」的替换证明。（用 `*(&acc) = 1` 而不是 `use(&acc)`：
        // 前者不需要额外声明，不会被「隐式声明」的语义错误抢在断言之前。）
        assert_eq!(
            escapes(
                "int outer(void){ int acc = 0; auto void f(void){ *(&acc) = 1; } f(); return acc; }\n",
                "outer"
            ),
            ["acc"],
            "嵌套函数体里的 &acc 不得漏报"
        );
        // 反向对照：没有任何逸出时集合必须为空。
        assert!(
            escapes("int f(void){ int x = 1; return x + 1; }\n", "f").is_empty(),
            "不取址时不得报逸出"
        );
    }
}
