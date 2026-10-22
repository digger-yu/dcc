// src/ast_dispatch.rs —— 跳表分派的**函数级**事实（阶段 6 批 4）。
//
// 为什么要有它：`goto *表[下标]`（computed goto 解释器的分派，以及批 1 把稠密 switch
// 改写出来的那一跳）今天是三条指令：`lea 表基址,%r11` + `mov (%r11,%rcx,8),%rax` +
// `jmp *%rax`。表是**静态局部**，它的地址在函数范围内是不变量，所以基址可以常驻一个
// 寄存器，三条塌成一条 `jmp *(%rbp,%rcx,8)`（实测 gcc -O2 在同一位置就是这一条）。
//
// 要动一个 callee-saved 号，就得在**发射开始之前**知道这个函数里有没有别的东西可能毁它
// —— 与 `ast_asm.rs` 那条同一纪律（设计 §3 H1：家在发射前定完；事后发现就晚了）。
// 本模块因此把三件事一次遍历收齐：
//   * `any_asm`：函数里出现过任何内联汇编（语句级 `Stmt::AsmGoto` 或表达式级
//     `Expr::AsmExpr`）。%rbp 已被本后端占作表基址，用户 asm 里怎么写它都无法追溯
//     （文本模板没有结构化名单），所以只要有 asm 就不占 —— 保守方向。
//   * `any_nested_func`：嵌套函数体的发射横跨外层函数的文本区，外层的 `%rbp` 约定
//     穿不过去（与 `ssa::regalloc` 的「有嵌套函数就整体不绑家」同一口径）。
//   * `tables`：每张被 `goto *表[…]` 读过的表的源码名与站点数，按首次出现顺序
//     （顺序可复现 —— 产物可复现性是本仓门禁项，HashMap 迭代序不能用）。
//
// **只回答形态，不回答符号**：「这个名字是不是本函数的静态局部」是后端注册表的事实
// （`StaticLocalMap`），留在这里就得把那张表搬进来。
//
// 遍历刻意不留宽松的 `_ => {}`：新增 `Stmt`/`Expr` 变体会直接编不过。漏收一条 asm 的
// 代价是「本该让开的 %rbp 被 asm 毁掉」，那是静默错值那一类。

use crate::ast::{Block, BlockItem, Case, Decl, Expr, InitItem, Stmt, Type};

/// `goto *表[下标]` 中那张表的一次读取（形态判据的单一出处）。
///
/// 发射器（`emit/stmt.rs` 的 `GotoIndirect` 臂）与本收集器都调 [`indirect_table_site`]，
/// 两边不可能认得不一样 —— 认出形状却不按这条路发，或反之，都是分派跳到别的函数去。
pub struct TableSite<'a> {
    /// 表名的**源码**拼写（还没 mangle）
    pub table: &'a str,
    /// 下标表达式（原样，交给发射器求值）
    pub idx: &'a Expr,
    /// 元素类型
    pub elem: &'a Type,
}

/// 把 `goto *expr` 的操作数认成「一张静态局部指针表的一次下标读」。
///
/// 接受的形态刻意很窄：只剥「值转换」层的 `Cast`（`(void *)tbl[i]` 仍是同一张表的读），
/// 然后要求 `Index{arr: Ident, idx, elem}` 且元素是**指针**。多维下标、基址来自指针变量
/// （`p[i]`）、函数指针数组的元素类型不是 Ptr 等等一律返 `None`。
pub fn indirect_table_site(expr: &Expr) -> Option<TableSite<'_>> {
    let mut cur = expr;
    while let Expr::Cast { e: inner, .. } = cur {
        cur = inner;
    }
    if let Expr::Index { arr, idx, ty, .. } = cur {
        if let Expr::Ident { name, .. } = arr.as_ref() {
            // 只有指针元素才是「跳表的一项」；尺寸还得能进 SIB 的 scale（1/2/4/8）。
            if matches!(ty, Type::Ptr(_)) && matches!(ty.size(), 1 | 2 | 4 | 8) {
                return Some(TableSite {
                    table: &name.name,
                    idx,
                    elem: ty,
                });
            }
        }
    }
    None
}

/// 一个函数体的分派事实。
pub struct DispatchFacts {
    pub any_asm: bool,
    pub any_nested_func: bool,
    /// (表源码名, `goto *表[…]` 站点数)，按首次出现顺序去重后计数。
    pub tables: Vec<(String, u32)>,
}

impl DispatchFacts {
    /// 站点最多的那张表；并列时取**最先出现**的。
    ///
    /// 为什么这样排：Lua 的 `disptab` 被每个 opcode 尾部的 `donext()` 展开成几十个站点，
    /// 静态计数就把它选出来；SQLite `sqlite3VdbeExec` 的主分派与它内部那些小 switch
    /// 各自只有一个站点，并列时「源码序在前」正好是主分派（它在函数体最前面）。
    /// 两个方向都不需要运行期剖面，也就不会因为某次负载不同而换默认。
    pub fn primary_table(&self) -> Option<&str> {
        let mut best: Option<(&str, u32)> = None;
        for (name, n) in &self.tables {
            if best.map_or(true, |(_, bn)| *n > bn) {
                best = Some((name.as_str(), *n));
            }
        }
        best.map(|(n, _)| n)
    }
}

/// 收集一个函数体的分派事实。嵌套函数体也算进来（它就在同一段 .text 里）。
pub fn collect(body: &Block) -> DispatchFacts {
    let mut out = DispatchFacts {
        any_asm: false,
        any_nested_func: false,
        tables: Vec::new(),
    };
    walk_block(body, &mut out);
    out
}

fn note_table(out: &mut DispatchFacts, name: &str) {
    if let Some(slot) = out.tables.iter_mut().find(|(n, _)| n == name) {
        slot.1 += 1;
    } else {
        out.tables.push((name.to_string(), 1));
    }
}

fn walk_block(b: &Block, out: &mut DispatchFacts) {
    for item in &b.items {
        match item {
            BlockItem::Stmt(st) => walk_stmt(st, out),
            BlockItem::Decl(d) => walk_decl(d, out),
        }
    }
}

fn walk_decl(d: &Decl, out: &mut DispatchFacts) {
    match d {
        Decl::Var(v) => {
            if let Some(e) = &v.init {
                walk_expr(e, out);
            }
        },
        Decl::Func(_) => {
            out.any_nested_func = true;
            // **不**递归进嵌套函数的体：本模块的两个消费者要的都是「本函数自己的事实」。
            // 发射端（`decide_disptab_base_reg`）看到 `any_nested_func` 就整函数拒掉，里面的
            // 站点与 asm 再计进来也不影响结论；内联端（`opt.rs::has_threaded_dispatch`）则否则
            // 会让一个自己不带 `goto *` 的外层 caller 因为嵌套函数里有一张跳表而拿到
            // 「不管多大都走臂体」那一条无上限通道。
        },
        // 这三类里没有表达式（类型/枚举定义不携带 asm）。`Decl::GlobalAsm` 只有文件级
        // 裸文本、没有操作数也不在函数体内 —— 但它确实会出现在 .text 里，而它的文本
        // 与本后端占用的号无关（它没有函数上下文），所以不进本判据。
        Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_) | Decl::Typedef { .. } => {},
        Decl::GlobalAsm { .. } => {},
    }
}

fn walk_stmt(st: &Stmt, out: &mut DispatchFacts) {
    match st {
        Stmt::Expr { e, .. } => walk_expr(e, out),
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                walk_expr(e, out);
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            walk_expr(cond, out);
            walk_stmt(then_, out);
            if let Some(x) = else_ {
                walk_stmt(x, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            walk_expr(cond, out);
            walk_stmt(body, out);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                walk_stmt(i, out);
            }
            if let Some(c) = cond {
                walk_expr(c, out);
            }
            if let Some(s) = step {
                walk_expr(s, out);
            }
            walk_stmt(body, out);
        },
        Stmt::Switch {
            expr,
            cases,
            prelude,
            ..
        } => {
            walk_expr(expr, out);
            for c in cases {
                walk_case(c, out);
            }
            for it in prelude {
                walk_block_item(it, out);
            }
        },
        Stmt::Block { b, .. } => walk_block(b, out),
        Stmt::Label { stmt: inner, .. } => walk_stmt(inner, out),
        Stmt::GotoIndirect { expr, .. } => {
            if let Some(site) = indirect_table_site(expr) {
                note_table(out, site.table);
            }
            walk_expr(expr, out);
        },
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            // 语句级 asm 一律算「本函数有 asm」，不管它有没有跳出目标：
            // 模板文本里写什么号是不可追溯的，而 %rbp 现在归本后端。
            out.any_asm = true;
            for e in outputs.iter().chain(inputs.iter()) {
                walk_expr(e, out);
            }
        },
        Stmt::Invoke { call, .. } => walk_stmt(call, out),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            walk_expr(object, out);
            walk_expr(selector, out);
        },
        Stmt::Goto { .. } | Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Fallthrough { .. } => {},
    }
}

fn walk_block_item(item: &BlockItem, out: &mut DispatchFacts) {
    match item {
        BlockItem::Stmt(st) => walk_stmt(st, out),
        BlockItem::Decl(d) => walk_decl(d, out),
    }
}

fn walk_case(c: &Case, out: &mut DispatchFacts) {
    if let Some(e) = &c.val {
        walk_expr(e, out);
    }
    if let Some(e) = &c.range_end {
        walk_expr(e, out);
    }
    walk_block(&c.body, out);
}

fn walk_expr(e: &Expr, out: &mut DispatchFacts) {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::Offsetof { .. }
        | Expr::LabelAddr { .. }
        | Expr::Ident { .. } => {},
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => walk_expr(x, out),
        Expr::Member { expr: x, .. } => walk_expr(x, out),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            walk_expr(l, out);
            walk_expr(r, out);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            walk_expr(cond, out);
            walk_expr(then_, out);
            walk_expr(else_, out);
        },
        Expr::Call { callee, args, .. } => {
            walk_expr(callee, out);
            for a in args {
                walk_expr(a, out);
            }
        },
        Expr::StmtExpr { block, .. } => walk_block(block, out),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => walk_expr(x, out),
                    InitItem::Designated { value, .. } => walk_expr(value, out),
                }
            }
        },
        Expr::VaArg { ap, .. } => walk_expr(ap, out),
        Expr::VaArgPack { .. } => {},
        Expr::AsmExpr { outputs, inputs, .. } => {
            out.any_asm = true;
            for x in outputs.iter().chain(inputs.iter()) {
                walk_expr(x, out);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_of(src: &str) -> (bool, bool, Vec<(String, u32)>) {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        for d in m.source().decls.iter() {
            if let Decl::Func(f) = d {
                if f.name.name == "f" {
                    let b = f.body.as_ref().expect("测试源里 f 必须有体");
                    let fa = collect(b);
                    return (fa.any_asm, fa.any_nested_func, fa.tables);
                }
            }
        }
        panic!("测试源里没有 f");
    }

    /// `goto *tbl[i]`（tbl 是指针数组）要收到表名与站点数；同一张表多个站点要累加。
    #[test]
    fn indirect_goto_on_pointer_array_is_collected() {
        let (asm, nested, tables) = facts_of(
            "void *tbl[4];\n\
             long f(long i){ if (i) goto *tbl[i]; return i; }",
        );
        assert!(!asm && !nested);
        assert_eq!(tables, vec![("tbl".to_string(), 1u32)]);
    }

    /// 只有「下标直接坐在 `goto *` 上」才算站点：先把项存进变量再 `goto *p` 不是
    /// （那条路上基址已经不在指令流里，批 4 的一条 `jmp *tbl(,idx,8)` 套不上）。
    #[test]
    fn stored_element_is_not_a_site() {
        let (_a, _n, tables) = facts_of(
            "void *tbl[4];\n\
             long f(long i){ void *p = tbl[i]; goto *p; }",
        );
        assert!(tables.is_empty(), "存过一道变量的 goto *p 不是表站点：{tables:?}");
    }

    /// 只有「指针元素的下标读」才算跳表站点：整数数组与指针变量都不算。
    #[test]
    fn non_pointer_and_dynamic_bases_are_not_tables() {
        let (_asm, _nested, tables) = facts_of(
            "long tbl[4]; long *p;\n\
             long f(long i){ long v = p[i]; goto *v ? (void*)0 : (void*)0; return i; }",
        );
        // `p[i]` 的 arr 不是数组而是指针变量，整条 shape 判据都不该成立
        assert!(tables.iter().all(|(n, _)| n != "p"), "指针变量的下标读不是表：{tables:?}");
    }

    /// 任何形态的 asm 都要置 `any_asm`（语句级、带跳出目标的、表达式级）。
    #[test]
    fn every_asm_shape_sets_the_flag() {
        for src in [
            "long f(long n){ __asm__ volatile(\"\" : : : \"rbx\"); return n; }",
            "long f(long n){ __asm__ goto(\"jmp %l0\" : : : \"rbx\" : o); o: return n; }",
            "long f(long n){ return ({ __asm__ volatile(\"\" : :); n; }); }",
        ] {
            let (asm, _n, _t) = facts_of(src);
            assert!(asm, "这条 asm 没被收到：{src}");
        }
    }

    /// 嵌套函数要置 `any_nested_func`（外层的 %rbp 约定穿不过内层的文本）。
    #[test]
    fn nested_function_is_reported() {
        let (_asm, nested, _t) =
            facts_of("long f(long n){ long g(long z){ return z+1; } return g(n); }");
        assert!(nested);
    }

    /// 表藏在循环体/switch 分支/语句表达式里也要收到，且站点数按出现顺序累计。
    #[test]
    fn table_sites_at_every_nesting_shape() {
        let (_a, _n, tables) = facts_of(
            "void *tbl[4];\n\
             long f(long i){ for(;;){ goto *tbl[i]; } return i; }",
        );
        assert_eq!(tables, vec![("tbl".to_string(), 1u32)]);
        let (_a, _n, tables2) = facts_of(
            "void *t2[4];\n\
             long f(long i){ switch(i){ case 1: goto *t2[i]; break; default: goto *t2[0]; } return i; }",
        );
        assert_eq!(tables2, vec![("t2".to_string(), 2u32)]);
    }

    /// `primary_table` 取站点最多的那张；并列时取最先出现的（可复现，且不依赖运行期剖面）。
    #[test]
    fn primary_table_prefers_most_sites_then_first() {
        let f = |src: &str| {
            let (_a, _n, tables) = facts_of(src);
            let d = DispatchFacts {
                any_asm: false,
                any_nested_func: false,
                tables,
            };
            let got = d.primary_table().map(|s| s.to_string());
            got
        };
        assert_eq!(
            f(
                "void *a[4]; void *b[4];\n\
                 long f(long i){ goto *a[i]; goto *a[i]; goto *b[i]; return i; }"
            )
            .as_deref(),
            Some("a")
        );
        assert_eq!(
            f(
                "void *a[4]; void *b[4];\n\
                 long f(long i){ goto *a[i]; goto *b[i]; return i; }"
            )
            .as_deref(),
            Some("a")
        );
        assert_eq!(f("long f(long i){ return i; }").as_deref(), None);
    }
}
