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

/// P16 软件流水的执行级守门夹具（流水已默认接入 -O2/-O3）。
///
/// 曾经默认关闭的两条实测缺陷（kernel 内 store 实例越界写、链尾多累加一轮）同源于
/// 一个根因：SSA→AST 重建把同基名塌缩成一个槽，而模调度的 T 序模型不表达这种
/// 反依赖。现在由 `ssa_pipelining::emitted_reads_match` 在发射序上过一道判定，排不出
/// 合法序的循环直接拒收（丢收益，不出错值）。
///
/// 这三条夹具的分工：
///   * `ssa_pipe_chain.c`：该流水的（依赖链）与不该流水的（归约/短 trip）共存，
///     逐档与 gcc 对照 ⇒ 错值与错拒都藏不住；
///   * `ssa_pipe_reduction.c`：被真流水的 load 归约（`red8_scaled`，trip=8、depth=1）
///     仍须与 gcc 一致；
///   * `ssa_pipe_nested.c`：锁住「嵌套场景不得破坏外层结构」这条准入判据。
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

/// 三种流水判定并存的样本：该流水的（两条 6 级依赖链）与不该流水的（load 归约
/// depth=0、5 级链 depth=0、trip 太短）放在同一个翻译单元里，默认路径逐档与 gcc 对照。
#[test]
fn ssa_pipe_chain_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-pipe-chain",
        include_str!("../fixtures/opt/ssa_pipe_chain.c"),
        "流水落地或准入判定改变了行为",
    );
}

/// 同一份源码、同一档位，只让 P16 开/关（`DCC_SSA_OFF=pipe`），返回 -O2 汇编。
fn dcc_asm_with_pipe(name: &str, src: &str, ssa_off: Option<&str>) -> String {
    let dir = env::temp_dir();
    let sp = dir.join(format!("dcc-{name}-pipe-{}.c", std::process::id()));
    let ap = dir.join(format!("dcc-{name}-pipe-{}.s", std::process::id()));
    fs::write(&sp, src).expect("写夹具");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-O2").args(["-S", "-o"]).arg(&ap).arg(&sp);
    if let Some(v) = ssa_off {
        cmd.env("DCC_SSA_OFF", v);
    }
    let out = cmd.output().expect("跑 dcc");
    assert!(out.status.success(), "编译失败：{}", String::from_utf8_lossy(&out.stderr));
    let text = fs::read_to_string(&ap).unwrap_or_default();
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&ap);
    text
}

/// 「pass 已接入」必须由**产物差异**证明，不能由代码已调用证明 —— 否则它可以是一个
/// 永远不生效的默认参数，而上面所有夹具与全部门禁依旧全绿（§7.5 就是这么发现的：
/// 接入点在管线末尾时，开/关的 -O2 汇编 sha256 完全相同）。
#[test]
fn ssa_pipe_default_on_changes_the_product() {
    let src = include_str!("../fixtures/opt/ssa_pipe_chain.c");
    let on = dcc_asm_with_pipe("pipe-chain", src, None);
    let off = dcc_asm_with_pipe("pipe-chain", src, Some("pipe"));
    assert_ne!(
        on, off,
        "默认路径的 -O2 产物必须因流水而不同；与 DCC_SSA_OFF=pipe 逐字节相同 ⇒ 本 pass 空转"
    );
}
