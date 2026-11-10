// src/ast_asm.rs —— 内联汇编对物理寄存器的**破坏事实**（AST 级，一次遍历）。
//
// 为什么要有它（`docs/p12-physreg-spill-design.md` §7 的 5.2：「干涉对象必须包括本条 asm
// 的操作数与 clobber」）：clobber 名单是用户向编译器宣告「这条 asm 之后这些寄存器的内容
// 不保证」。今天有两处读不到这个宣告，都会静默丢值（两条都是本轮实测出来的）：
//   * 自动分配把变量的家放在某个 callee-saved 号上，而函数里有一条显式 clobber 该号的 asm；
//   * 表达式栈机把中间值溅在同一个号上，跨同一条 asm 读回来。
//
// 大部分 clobber 只需要在**发射点**处理：asm 就在那儿，前后插一对保存/恢复就能守住
// （见 `codegen::x86_64::asm::emit`）。唯独 `asm goto` 不行 —— 它可以把控制流带出那个区间，
// 跳走的那条路上没有恢复。所以这件事必须在函数级的**发射开始之前**就知道（设计 §3 的 H1：
// 家在发射前定完），于是它是一次 AST 遍历的事实，而不是一个发射点的判据。
//
// 本模块只回答「哪些名字被一条能带出区间的 asm 宣告为已毁」。**不**做名字→编号的映射：
// 那是各消费方自己的 ISA 事实（分配器：`ssa::regalloc::pooled_reg_of`；
// 后端：`Codegen::parse_reg_name`），在这里映一次就会造出第四张号表。
//
// 遍历刻意不留宽松的 `_ => {}`：新增 `Stmt`/`Expr` 变体会直接编不过，而不是静默漏收一条
// asm —— 漏收的代价是「本该让开的寄存器被当成临时值用掉」，那是静默错值那一类。

use crate::ast::{Block, BlockItem, Case, Decl, Expr, InitItem, Stmt};
use std::collections::HashSet;

/// 本函数里「能把控制流带出保存/恢复区间」的那些 asm 所 clobber 的寄存器名。
///
/// 名字已规范化（去掉 `%` 前缀、转小写），与 `register asm` 的写法同一形态
/// （`asm("rbx")` 与 `::: "rbx"` / `::: "%rbx"` 都归到 `"rbx"`）。
///
/// 判据是 `Stmt::AsmGoto` 且 `goto_labels` 非空：没有目标标签的 `asm goto` 不会跳转，
/// 与普通的 `asm volatile` 一样能被 fall-through 守护覆盖，因此**不**进这份名单
/// （把它算进去会让函数白白失去一个可用的溅出号 —— 保守的方向也要有边界）。
///
/// 嵌套函数体也算进来（与 `ssa::regalloc` 的「有嵌套函数就整体不绑」同一口径）：
/// 外层函数的表达式区间可能横跨内层函数体的发射点，少看一条 asm 就是少让一个号。
pub fn escaping_asm_clobbers(body: &Block) -> HashSet<String> {
    collect_asm_facts(body).clobbers
}

/// 一次遍历收齐的**函数级** asm 事实。分两处收是因为两处都要「发射开始前就知道」：
/// 号让谁、`cld` 省不省，都是晚一步就来不及的判据。
#[derive(Default)]
pub struct AsmFacts {
    /// 见 [`escaping_asm_clobbers`]：能带出保存/恢复区间的 `asm goto` 所 clobber 的名字。
    pub clobbers: HashSet<String>,
    /// 本函数体里**任何**一条内联汇编的模板出现过 `std` 这个词。DF 一旦被它弄脏，
    /// 之后每一条 `rep movs`/`rep stos` 前的 `cld` 都必须保留（省掉就是错值）；
    /// 消费方是 `Codegen::emit_direction_clear`。
    pub sets_direction: bool,
    /// 本函数体里**任何**一条内联汇编的约束文本（outputs + inputs，原始写法）。
    ///
    /// 消费方是 `Codegen::compute_callee_plan`（报告 §5.42）：帧头的 callee-saved
    /// 保存槽现在是按需的，而 `"b"`（或 `{rbx}` 这类显式名）约束会把某个池号**钉死**
    /// 在本函数的产物里 —— 那个号必须提前进计划，否则序言没有它的保存槽。
    pub constraints: Vec<String>,
    /// 本函数里出现过内联汇编（任何形态）。
    ///
    /// 消费方同上：asm 的内存操作数地址寄存器走 `alloc_asm_mem_scratch`，池耗尽时
    /// 回退到 r12（旧行为）。为了让那条回退仍然被保存槽盖住，含 asm 的函数
    /// 至少把 r12 留在计划里。
    pub has_asm: bool,
}

/// 收集 [`AsmFacts`]。刻意不拆成两次遍历：两张判据看的是同一棵 AST，分开走就会
/// 有一侧漏掉某个新加的变体（这里的遍历是穷举匹配、没有宽松的 `_ => {}`）。
pub fn collect_asm_facts(body: &Block) -> AsmFacts {
    let mut out = AsmFacts::default();
    walk_block(body, &mut out);
    out
}

/// asm 模板里是否把方向标志置位。按词切（`%`/空格/`,`/`;`/`(` 都是分隔符），
/// 免把 `stdin`、`.hidden` 这类子串当成 `std`。
fn template_sets_direction(t: &str) -> bool {
    t.to_ascii_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w == "std")
}

/// 规范化一个 clobber 名字。与 `register asm` 的名字口径保持一致，
/// 这样两个消费方拿到的字符串可以直接与各自的号表比对。
pub fn normalize_clobber_name(raw: &str) -> String {
    raw.trim().trim_start_matches('%').to_ascii_lowercase()
}

fn walk_block(b: &Block, out: &mut AsmFacts) {
    walk_items(&b.items, out)
}

/// 项列表层面：`Stmt::Switch` 除了 `cases` 还有一份 `prelude`，它不是一个 `Block`，
/// 所以这一层必须能单独走（漏了它就是静默少扫一棵子树）。
fn walk_items(items: &[BlockItem], out: &mut AsmFacts) {
    for item in items {
        match item {
            BlockItem::Stmt(st) => walk_stmt(st, out),
            BlockItem::Decl(d) => walk_decl(d, out),
        }
    }
}

fn walk_decl(d: &Decl, out: &mut AsmFacts) {
    match d {
        Decl::Var(v) => {
            if let Some(e) = &v.init {
                walk_expr(e, out);
            }
        },
        Decl::Func(f) => {
            if let Some(b) = &f.body {
                walk_block(b, out);
            }
        },
        // 这三类里没有表达式（类型/枚举定义不携带 asm），GlobalAsm 只有裸文本、
        // 没有操作数也没有 clobber 名单。
        Decl::StructDef(_)
        | Decl::UnionDef(_)
        | Decl::EnumDef(_)
        | Decl::Typedef { .. }
        | Decl::GlobalAsm { .. } => {},
    }
}

fn walk_stmt(st: &Stmt, out: &mut AsmFacts) {
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
            walk_items(prelude, out);
            for c in cases {
                walk_case(c, out);
            }
        },
        Stmt::Block { b, .. } => walk_block(b, out),
        Stmt::Label { stmt: inner, .. } => walk_stmt(inner, out),
        Stmt::GotoIndirect { expr, .. } => walk_expr(expr, out),
        Stmt::AsmGoto {
            asm_str,
            clobbers,
            goto_labels,
            outputs,
            inputs,
            output_constraints,
            input_constraints,
            ..
        } => {
            out.sets_direction |= template_sets_direction(asm_str);
            out.has_asm = true;
            out.constraints.extend(output_constraints.iter().cloned());
            out.constraints.extend(input_constraints.iter().cloned());
            // 只有真的带着跳出目标才算「逃得出守护区间」。
            if !goto_labels.is_empty() {
                for c in clobbers {
                    out.clobbers.insert(normalize_clobber_name(c));
                }
            }
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
        Stmt::Goto { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => {},
    }
}

fn walk_case(c: &Case, out: &mut AsmFacts) {
    if let Some(e) = &c.val {
        walk_expr(e, out);
    }
    if let Some(e) = &c.range_end {
        walk_expr(e, out);
    }
    walk_block(&c.body, out);
}

fn walk_expr(e: &Expr, out: &mut AsmFacts) {
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
        // 普通的 `asm`（含 `Expr::AsmExpr` 形态的语句）永远能被 fall-through 守护覆盖：
        // 它没有跳出目标，所以只进发射点的名单，不进本函数级判据。操作数里仍可能有 asm。
        Expr::AsmExpr {
            asm_str,
            outputs,
            inputs,
            output_constraints,
            input_constraints,
            ..
        } => {
            out.sets_direction |= template_sets_direction(asm_str);
            out.has_asm = true;
            out.constraints.extend(output_constraints.iter().cloned());
            out.constraints.extend(input_constraints.iter().cloned());
            for x in outputs.iter().chain(inputs.iter()) {
                walk_expr(x, out);
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clobbers_of(src: &str) -> Vec<String> {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let mut out: Vec<String> = Vec::new();
        for d in &m.source().decls {
            if let Decl::Func(f) = d {
                if let Some(b) = &f.body {
                    for n in escaping_asm_clobbers(b) {
                        out.push(n);
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// `asm goto` 带跳出目标 ⇒ 名单里必须有它 clobber 的号；`%` 前缀与大小写不得影响判定。
    #[test]
    fn asm_goto_with_labels_reports_its_clobbers() {
        let v = clobbers_of(
            "long f(long n){ if (n) __asm__ goto(\"jmp %l0\" : : : \"%RBX\" : out); out: return n; }",
        );
        assert_eq!(v, vec!["rbx".to_string()], "跳得出的 asm 必须让开 %rbx，实际 {v:?}");
    }

    /// 反向对照（判据不得退化成「看见 asm goto 就禁用」）：没有标签的 `asm goto` 不跳转，
    /// 与 `asm volatile` 等价，守护区间覆盖得住。
    #[test]
    fn asm_goto_without_labels_is_not_an_escape() {
        let v = clobbers_of("long f(long n){ __asm__ goto(\"\" : : : \"rbx\"); return n; }");
        assert!(v.is_empty(), "无跳出目标不该进逃逸名单，实际 {v:?}");
        // 普通 asm 也一样不进本判据（它在发射点由保存/恢复处理）
        let w = clobbers_of("long f(long n){ __asm__ volatile(\"\" : : : \"r12\"); return n; }");
        assert!(w.is_empty(), "普通 asm 不该进逃逸名单，实际 {w:?}");
    }

    /// 逃逸 asm 藏在**深处**也必须收到：语句表达式、循环体、switch 分支、嵌套函数体。
    /// 每一条都是一个真实的落点形状，漏收的那条就是静默错值。
    #[test]
    fn escape_is_found_at_every_nesting_shape() {
        let cases: [(&str, &[&str]); 4] = [
            (
                "long f(long n){ return (n+1) * ({ __asm__ goto(\"jmp %l0\" : : : \"r14\" : o); o: n; }); }",
                &["r14"][..],
            ),
            (
                "long f(long n){ for(;;){ __asm__ goto(\"jmp %l0\" : : : \"r15\" : o); } o: return n; }",
                &["r15"][..],
            ),
            (
                "long f(long n){ switch(n){ case 1: __asm__ goto(\"jmp %l0\" : : : \"rbx\" : o); break; default: break; } o: return n; }",
                &["rbx"][..],
            ),
            (
                "long f(long n){ long g(long z){ __asm__ goto(\"jmp %l0\" : : : \"r12\" : o2); o2: return z; } return g(n); }",
                &["r12"][..],
            ),
        ];
        for (src, want) in cases {
            let got = clobbers_of(src);
            assert_eq!(got, want.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "源：{src}");
        }
    }

    /// 规范化：clobber 名单里的写法五花八门，比对口径只有一种。
    #[test]
    fn clobber_names_are_normalized() {
        assert_eq!(normalize_clobber_name("%rbx"), "rbx");
        assert_eq!(normalize_clobber_name(" RBX "), "rbx");
        assert_eq!(normalize_clobber_name("memory"), "memory");
    }
}
