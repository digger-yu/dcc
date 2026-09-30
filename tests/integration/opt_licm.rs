// included from tests/integration.rs — not a standalone cargo target

// ===== LICM（循环不变式外提）执行级门禁 =====
//
// 这一组测试补的是本项目门禁最大的空洞：`tests/integration/opt.rs` 里的 m16_*
// 「优化测试」全部走 `run_compile_and_run`（零参数 = **-O0**），于是它们证明的是
// 「源码本身是对的」，而不是「优化器是对的」。同一批夹具手动加 `-O2` 后曾有 5 个
// SEGV / HANG / 错值，而 `make test` 全绿。
//
// 因此这里的每一条都必须：
//   1. 带 `-O2` / `-O3` 真正跑一遍优化管线；
//   2. 以 **gcc 的 stdout + 退出码** 为参照（`run_gcc_cross_check_opt`），而不是
//      硬编码「返回 0」——两个编译器「一致地错」时硬编码断言照样通过；
//   3. 运行有超时上限（`run_exe_bounded`）：LICM 缺陷的失效形态是把 while 体掏空成
//      忙等，无上限时 `cargo test` 会被吊死而不是跑红。
//
// 夹具来源（本次修复的根因，逐条对应）：
//   licm_while_carried   `Stmt::While` 分支把 loop_var 传成 `None` ⇒ `expr_mentions`
//                        恒「不涉及」⇒ 整个循环体被判不变式外提，`while(n>0){}` 忙等。
//   licm_do_while_carried 同一条判据的 do-while 形态（三条分支必须共用一个收集器）。
//   licm_hoist_order     外提语句被 prepend 到块首 ⇒ 块内语句序（栈槽/产物字段的
//                        「顺序单一真源」）被打破，`q()` 一类函数直接错值。
//   licm_invariant_for   真不变式**应当**被外提且结果分毫不变（防止把修复做成
//                        「干脆不做 LICM」的假通过）。
//   licm_call_global     体内有调用 ⇒ 全局/逃逸对象必须进名字栅栏，不得外提。
//   licm_volatile_read   volatile 读本身就是可观测副作用，与有没有调用无关。

#[test]
fn licm_while_carried_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/licm_while_carried.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("licm-while-carried", src, &[flag]),
            0,
            "{flag}: licm_while_carried.c 应与 gcc 一致 (while 体被整体外提 ⇒ 忙等/错值)"
        );
    }
}

#[test]
fn licm_do_while_carried_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/licm_do_while_carried.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("licm-do-while-carried", src, &[flag]),
            0,
            "{flag}: do-while 形态必须与 while 走同一个不变式判据"
        );
    }
}

#[test]
fn licm_hoist_order_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/licm_hoist_order.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("licm-hoist-order", src, &[flag]),
            0,
            "{flag}: 外提语句必须插回原循环所在下标之前，不得 prepend 到块首"
        );
    }
}

#[test]
fn licm_invariant_for_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/licm_invariant_for.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("licm-invariant-for", src, &[flag]),
            0,
            "{flag}: 真正的循环不变式应被外提且结果不变"
        );
    }
}

#[test]
fn licm_call_global_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/licm_call_global.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("licm-call-global", src, &[flag]),
            0,
            "{flag}: 体内含调用/间接写时，读全局与被取址局部的声明不得外提"
        );
    }
}

#[test]
fn licm_volatile_read_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/licm_volatile_read.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("licm-volatile-read", src, &[flag]),
            0,
            "{flag}: volatile 读是不可观测消除的副作用"
        );
    }
}

/// LICM 的**结构性**门禁：外提后循环体不能被搬空。
///
/// 为什么行为等价（上面的退出码对照）不足以覆盖：`while (n > 0) {}` 这种忙等在
/// 「循环本来就不执行」的输入上与正确代码等价，退出码照过；只有把语句级形态
/// 钉住，才能防住「判据重新退化成恒真」这一整类回归。
///
/// 判据建立在 IR 形态上而不是汇编文本上：x86_64 的 `-S` 目前由反汇编器渲染，
/// 分支目标会被写成 `<dcc-unresolved-target-...>`（`src/codegen/disasm.rs`），
/// 拿它当断言依据会先把工具自身的缺陷当成编译器的缺陷。
#[test]
fn licm_never_empties_a_while_body() {
    let src = include_str!("../fixtures/opt/licm_while_carried.c");
    // -O2 与 -O3 都跑过 LICM；两者都必须保留循环体语句。
    for flag in ["-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("licm-structure", src, &[flag]);
        assert!(out.status.success(), "{flag}: 编译应成功");
        // 能编译并**在时限内**退出即证明没有空转循环：`sum_down(3)` 是有限步循环，
        // 一旦循环体被掏空成 `while (n > 0) {}` 就永不退出，`run_exe_bounded` 会 panic。
        let code = run_exe(&exe);
        assert_eq!(code, 0, "{flag}: 结构夹具退出码应为 0");
        let _ = fs::remove_file(&exe);
    }
}
