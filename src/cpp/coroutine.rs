//! 协程真状态机 (M26)
//!
//! 职责: 把含 `co_await` / `co_yield` / `co_return` 的函数体降级为
//! Duff's device 状态机 (switch + case 标签混排):
//!
//! ```c
//! static int <fn>_coro_state = 0;
//! static <ret> <fn>_coro_value;
//! static int <fn>_coro_done = 0;
//! switch (<fn>_coro_state) {
//! case 0:
//!     ...body...
//!     <fn>_coro_state = 1;
//!     <fn>_coro_value = (<expr>);
//!     return <fn>_coro_value;      // 挂起: 产出值给调用方
//! case 1:                          // 恢复点: 下次调用从这里继续
//!     ...
//! }
//! return 0;
//! ```
//!
//! 设计原则:
//! 1. 状态变量 per-function 命名 (mangled 名 + `_coro_state` 后缀),
//!    避免多协程函数互相干扰.
//! 2. 每次挂起递增状态号; `case N+1:` 紧跟挂起代码, 恢复时从下一条
//!    语句继续执行 (与 Duff's device 相同).
//! 3. 不依赖 lower 上下文, 纯字符串生成; 表达式由 lower 先渲染为
//!    C 文本再传入.

use crate::cpp::parser::{CppBody, CppStmt};

/// 状态变量名: `<fn>_coro_state`
pub fn coro_state_var(fn_name: &str) -> String {
    format!("{}_coro_state", fn_name)
}

/// 返回槽变量名: `<fn>_coro_value` (void 返回时不需要)
pub fn coro_value_var(fn_name: &str) -> String {
    format!("{}_coro_value", fn_name)
}

/// 完成标志变量名: `<fn>_coro_done`
pub fn coro_done_var(fn_name: &str) -> String {
    format!("{}_coro_done", fn_name)
}

/// 递归检测函数体是否含协程语句 (co_await / co_yield / co_return)
pub fn body_has_coro(body: &CppBody) -> bool {
    body.stmts.iter().any(stmt_has_coro)
}

fn stmt_has_coro(s: &CppStmt) -> bool {
    match s {
        CppStmt::CoAwait(_) | CppStmt::CoYield(_) | CppStmt::CoReturn(_) => true,
        CppStmt::If { then, els, .. } => {
            body_has_coro(then) || els.as_ref().map(|e| body_has_coro(e)).unwrap_or(false)
        }
        CppStmt::IfConstexpr { then, els, .. } => {
            body_has_coro(then) || els.as_ref().map(|e| body_has_coro(e)).unwrap_or(false)
        }
        CppStmt::While { body, .. } => body_has_coro(body),
        CppStmt::For { body, .. } => body_has_coro(body),
        CppStmt::Block(b) => body_has_coro(b),
        CppStmt::Try { body, catches } => {
            body_has_coro(body) || catches.iter().any(|c| body_has_coro(&c.body))
        }
        CppStmt::ThisMember(_, inner) => stmt_has_coro(inner),
        _ => false,
    }
}

/// 协程函数前导: per-function 状态变量 + switch 开头 + 初始入口 case 0.
/// `ret_c` 是返回类型的 C 字符串 ("void" 时省略返回槽变量).
pub fn coro_fn_prologue(fn_name: &str, ret_c: &str) -> String {
    let mut s = format!("static int {} = 0;\n", coro_state_var(fn_name));
    if ret_c != "void" {
        s.push_str(&format!("static {} {};\n", ret_c, coro_value_var(fn_name)));
    }
    s.push_str(&format!("static int {} = 0;\n", coro_done_var(fn_name)));
    s.push_str(&format!("switch ({}) {{\n", coro_state_var(fn_name)));
    s.push_str("case 0:\n");
    s
}

/// 协程函数收尾: 闭合 switch + 终态 return.
/// 终态 (state 越过最后一个 case) 返回 0 / 空, 表示协程已完成.
pub fn coro_fn_epilogue(ret_c: &str) -> String {
    let mut s = String::from("}\n");
    if ret_c == "void" {
        s.push_str("return;\n");
    } else {
        s.push_str("return 0;\n");
    }
    s
}

/// 挂起点代码 (co_await / co_yield):
/// `state = n+1; value = (expr); return value;` 后紧跟恢复标签 `case n+1:`.
/// `n` 是当前挂起点编号 (0 起); 下一次调用恢复在 case n+1, 继续执行
/// 挂起点之后的语句.
pub fn coro_suspend(fn_name: &str, ret_c: &str, n: i64, expr: &str) -> String {
    let st = coro_state_var(fn_name);
    let val = coro_value_var(fn_name);
    let body = if ret_c == "void" {
        format!(
            "    {} = {};\n    (void)({});\n    return;\n",
            st,
            n + 1,
            expr
        )
    } else {
        format!(
            "    {} = {};\n    {} = ({});\n    return {};\n",
            st,
            n + 1,
            val,
            expr,
            val
        )
    };
    format!("{}\n    case {}:\n", body, n + 1)
}

/// co_return 代码: 置完成标志, 存入返回槽并返回.
/// 不生成恢复标签 (协程已终止; 后续调用落入 epilogue 的终态 return).
pub fn coro_return(fn_name: &str, ret_c: &str, n: i64, expr: &str) -> String {
    let st = coro_state_var(fn_name);
    let done = coro_done_var(fn_name);
    let val = coro_value_var(fn_name);
    if ret_c == "void" {
        format!(
            "    {} = {};\n    {} = 1;\n    (void)({});\n    return;\n",
            st,
            n + 1,
            done,
            expr
        )
    } else {
        format!(
            "    {} = {};\n    {} = 1;\n    {} = ({});\n    return {};\n",
            st,
            n + 1,
            done,
            val,
            expr,
            val
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::{CppBody, CppExpr, CppStmt};

    fn body(stmts: Vec<CppStmt>) -> CppBody {
        CppBody { stmts }
    }

    #[test]
    fn body_has_coro_detects_top_level() {
        let b = body(vec![
            CppStmt::Expr(CppExpr::IntLit(1)),
            CppStmt::CoYield(CppExpr::IntLit(2)),
        ]);
        assert!(body_has_coro(&b));
    }

    #[test]
    fn body_has_coro_detects_nested_blocks() {
        // 协程语句嵌套在 if / while / block 内也应被检测到
        let inner = body(vec![CppStmt::CoAwait(CppExpr::IntLit(1))]);
        let b = body(vec![
            CppStmt::If {
                cond: CppExpr::IntLit(1),
                then: Box::new(inner.clone()),
                els: None,
            },
            CppStmt::While {
                cond: CppExpr::IntLit(0),
                body: Box::new(inner),
            },
        ]);
        assert!(body_has_coro(&b));
    }

    #[test]
    fn body_has_coro_returns_false_for_plain_fn() {
        let b = body(vec![CppStmt::Return(Some(CppExpr::IntLit(0)))]);
        assert!(!body_has_coro(&b));
    }

    #[test]
    fn suspend_emits_state_value_and_resume_label() {
        let s = coro_suspend("_Z1fv", "int", 0, "42");
        assert!(s.contains("_Z1fv_coro_state = 1;"), "{}", s);
        assert!(s.contains("_Z1fv_coro_value = (42);"), "{}", s);
        assert!(s.contains("return _Z1fv_coro_value;"), "{}", s);
        assert!(s.contains("case 1:"), "{}", s);
    }

    #[test]
    fn suspend_void_skips_value_slot() {
        let s = coro_suspend("_Z1fv", "void", 2, "x");
        assert!(s.contains("(void)(x);"), "{}", s);
        assert!(s.contains("return;"), "{}", s);
        assert!(!s.contains("_coro_value"), "{}", s);
        assert!(s.contains("case 3:"), "{}", s);
    }

    #[test]
    fn coro_return_marks_done_without_resume_label() {
        let s = coro_return("_Z1fv", "int", 1, "7");
        assert!(s.contains("_Z1fv_coro_state = 2;"), "{}", s);
        assert!(s.contains("_Z1fv_coro_done = 1;"), "{}", s);
        assert!(s.contains("return _Z1fv_coro_value;"), "{}", s);
        assert!(!s.contains("case"), "{}", s);
    }

    #[test]
    fn prologue_epilogue_pair_forms_valid_switch() {
        let p = coro_fn_prologue("_Z1fv", "int");
        let e = coro_fn_epilogue("int");
        assert!(p.contains("static int _Z1fv_coro_state = 0;"), "{}", p);
        assert!(p.contains("static int _Z1fv_coro_value;"), "{}", p);
        assert!(p.contains("static int _Z1fv_coro_done = 0;"), "{}", p);
        assert!(p.contains("switch (_Z1fv_coro_state) {"), "{}", p);
        assert!(p.contains("case 0:"), "{}", p);
        assert_eq!(e, "}\nreturn 0;\n");
    }
}
