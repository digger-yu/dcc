// included from tests/integration.rs — not a standalone cargo target

// ===== SSA 管线准入、重建顺序与循环形态的执行级门禁 =====
//
// 每条对应一类已观测到的静默误编译：
//
//   1. `switch` 降级为 if-chain 时的 `break` 改绑。旧实现直接把 `case.body`
//     拼成 if-else，于是原本跳出 switch 的 `break` 变成跳出**外层循环**（实测
//     `for(i<3){switch(i){case 0:break; ...}}` 在 -O2 返回 0而不是 3）。循环曾被
//     `func_has_loop` 闸门挡在 SSA 外，所以上述闸门拆除后这类才暴露。现在
//     `lower_switch_stmt` 把链包进 `do {…} while(0)` 并对 fallthrough / 绑定到
//     switch 的 `continue` 放弃降级。
//
//   2. 块内重排不得颠倒同一变量的多次赋值。本 IR 的 SSA→AST 重建按**基名塌缩**：
//      `y_1/y_2/y_3` 在 SSA 里互不相关，重建后都是变量 `y`，相对顺序即语义。本轮把
//      P14 改成「按依赖闭包上提」时曾把 `y = 20; y += 3;` 提到 `y = 5;` 之前，返回值
//      从 23 变 5 —— 由优化器自己制造的静默误编译，故留下一条顺序夹具。
//
//   3. 重建期内联一条临时值，等于把它的读**搬到使用点重算**。调度器拉开定义与使用
//      之间夹上对同基名的写入时就是错值（实测 `while(n>0){s+=step*n; n--;}` 在 -O3
//      少算一圈）。同一根因的第二形态：`while (k-- > 0)` 的自减写在节点头，而
//      `rebuild_cycle` 提出循环条件时会丢弃头块指令 ⇒ 循环永不退出。
//
//   4. 被取址的标量局部必须留在内存里（不能开 SSA 版本），否则指针间接写会丢。
//      这条与循环无关，只有按档位扫才看得见。
//
//   5. -O3 展开必须替换体内**所有**形态（嵌套 for/while/if/switch）的归纳变量；
//      并且不得把绑定到本循环的 `break`/`continue` 复制成悬空控制流。
//
//   6. AST 层 GVN 的 coalesce 删声明前必须看**全函数**的写入集合：
//      `for (int i = ti; i < 5; i++)` 的声明在 for-init 子块里，而 `i++` 属于外层
//      节点，块内视角看不到 ⇒ 声明被删后循环读一个从未赋值的 `i`。
//
//   7. 「合成临时值」不能拿名字前缀猜，也不能让 SSA 版本名与临时值 base 同名字空间：
//      `_s` 是合法用户标识符，而 `int _tbin` 的第 2 个版本 `_tbin_2` 会与
//      `fresh("bin")` 的临时值 base 完全同名 ⇒ 两个不相关的 SSA 名互相覆盖。
//
// 全部通过 `run_gcc_cross_check_opt` 运行：带 `-O` 档位、带超时、与 gcc 逐字节对照。

/// 逐档与 gcc 对照同一份夹具; 失效形态含 HANG, 故必须走有界运行。
fn assert_all_levels_match_gcc(tag: &str, src: &str, why: &str) {
    if !tool_exists("gcc") {
        return;
    }
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt(tag, src, &[flag]),
            0,
            "{flag}: {why}"
        );
    }
}

#[test]
fn ssa_gate_switch_loop_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-gate-switch-loop",
        include_str!("../fixtures/opt/ssa_gate_switch_loop.c"),
        "switch/label 里包的循环被 SSA 闸门漏判 ⇒ rebuild_cycle 出错",
    );
}

#[test]
fn ssa_order_multi_assign_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-order-multi-assign",
        include_str!("../fixtures/opt/ssa_order_multi_assign.c"),
        "块内重排颠倒了同一变量的多次赋值顺序",
    );
}

#[test]
fn ssa_loop_shapes_match_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-loop-shapes",
        include_str!("../fixtures/opt/ssa_loop_shapes.c"),
        "循环形态在优化档位下行为不一致",
    );
}

#[test]
fn ssa_rebuild_inline_order_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-rebuild-inline-order",
        include_str!("../fixtures/opt/ssa_rebuild_inline_order.c"),
        "重建期内联把临时值的读搬到了同基名写入之后 (或丢弃了节点头的写)",
    );
}

#[test]
fn ssa_addr_taken_local_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-addr-taken-local",
        include_str!("../fixtures/opt/ssa_addr_taken_local.c"),
        "被取址的标量局部被 SSA 版本化, 丢了指针的间接写",
    );
}

#[test]
fn ssa_unroll_nested_iv_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-unroll-nested-iv",
        include_str!("../fixtures/opt/ssa_unroll_nested_iv.c"),
        "-O3 展开没替换嵌套语句里的归纳变量, 或复制了悬空的 break/continue",
    );
}

#[test]
fn gvn_loop_copied_iv_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "gvn-loop-copied-iv",
        include_str!("../fixtures/opt/gvn_loop_copied_iv.c"),
        "AST 层 GVN 用块内写入集合判定, 删掉了循环变量的拷贝声明",
    );
}

#[test]
fn underscore_locals_match_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "underscore-locals",
        include_str!("../fixtures/opt/underscore_locals.c"),
        "下划线命名的用户变量被当成合成临时值, 或 SSA 版本名与临时值 base 撞名",
    );
}

/// P16 软件流水的两条执行级守门夹具。
///
/// 落地（peel + kernel 重排 + 一级轮转 phi + 出口侧最后实例）已实现，但**默认不接入
/// 产物**：进入产物后有一条已定位、尚未修好的缺陷 —— `main` 里的纯写循环被流水时
/// -O2 输出整块丢失（越界写踩坏 stdio，见 `ssa_passes.rs` 里 `DCC_PIPE_ON` 的注释）。
///
/// 这两条夹具在**默认路径**上盯住「各档位与 gcc 逐字节一致」，因此：
///   * 现在它们是绿的，证明默认路径没被落地污染；
///   * 把流水默认打开的那一刻，它们会立刻变红，直到 kernel 的 store 实例被修对。
/// `ssa_pipe_nested.c` 另外锁住「嵌套场景不得破坏外层结构」这条准入判据。
#[test]
fn ssa_pipe_reduction_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-pipe-reduction",
        include_str!("../fixtures/opt/ssa_pipe_reduction.c"),
        "P16 落地的 peel/kernel/轮转 phi 改变了归约语义",
    );
}

#[test]
fn ssa_pipe_nested_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-pipe-nested",
        include_str!("../fixtures/opt/ssa_pipe_nested.c"),
        "内层循环被流水时外层携带值丢失或外层推进链被破坏（曾表现为死循环）",
    );
}

/// 三种流水判定并存的样本：该流水的（依赖链）与不该流水的（load 归约 depth=0）
/// 放在同一个翻译单元里，默认路径逐档与 gcc 对照。
#[test]
fn ssa_pipe_chain_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-pipe-chain",
        include_str!("../fixtures/opt/ssa_pipe_chain.c"),
        "流水落地或准入判定改变了行为",
    );
}
