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
    let mut out = HashSet::new();
    walk_block(body, &mut out);
    out
}

/// 规范化一个 clobber 名字。与 `register asm` 的名字口径保持一致，
/// 这样两个消费方拿到的字符串可以直接与各自的号表比对。
pub fn normalize_clobber_name(raw: &str) -> String {
    raw.trim().trim_start_matches('%').to_ascii_lowercase()
}

fn walk_block(b: &Block, out: &mut HashSet<String>) {
    for item in &b.items {
        match item {
            BlockItem::Stmt(st) => walk_stmt(st, out),
            BlockItem::Decl(d) => walk_decl(d, out),
        }
    }
}

fn walk_decl(d: &Decl, out: &mut HashSet<String>) {
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
        Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_) | Decl::Typedef { .. }
        | Decl::GlobalAsm { .. } => {},
    }
}

fn walk_stmt(st: &Stmt, out: &mut HashSet<String>) {
    match st {
        Stmt::Expr { e, .. } => walk_expr(e, out),
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                walk_expr(e, out);
            }
        },
        Stmt::If { cond, then_, else_, .. } => {
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
        Stmt::Switch { expr, cases, .. } => {
            walk_expr(expr, out);
            for c in cases {
                walk_case(c, out);
            }
        },
        Stmt::Block { b, .. } => walk_block(b, out),
        Stmt::Label { stmt: inner, .. } => walk_stmt(inner, out),
        Stmt::GotoIndirect { expr, .. } => walk_expr(expr, out),
        Stmt::AsmGoto {
            clobbers,
            goto_labels,
            outputs,
            inputs,
            ..
        } => {
            // 只有真的带着跳出目标才算「逃得出守护区间」。
            if !goto_labels.is_empty() {
                for c in clobbers {
                    out.insert(normalize_clobber_name(c));
                }
            }
            for e in outputs.iter().chain(inputs.iter()) {
                walk_expr(e, out);
            }
        },
        Stmt::Invoke { call, .. } => walk_stmt(call, out),
        Stmt::EhLandingPad { object, selector, .. } => {
            walk_expr(object, out);
            walk_expr(selector, out);
        },
        Stmt::Goto { .. } | Stmt::Break { .. } | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => {},
    }
}

fn walk_case(c: &Case, out: &mut HashSet<String>) {
    if let Some(e) = &c.val {
        walk_expr(e, out);
    }
    if let Some(e) = &c.range_end {
        walk_expr(e, out);
    }
    walk_block(&c.body, out);
}

fn walk_expr(e: &Expr, out: &mut HashSet<String>) {
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
        Expr::Cond { cond, then_, else_, .. } => {
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
        // 普通的 `asm`（含 `Expr::AsmExpr` 形态的语句）永远能被 fall-through 守护覆盖：
        // 它没有跳出目标，所以只进发射点的名单，不进本函数级判据。操作数里仍可能有 asm。
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
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
