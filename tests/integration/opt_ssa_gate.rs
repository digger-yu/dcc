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

// ===== P12：SSA 层寄存器分配 =====
//
// `ssa_regalloc` 把热变量灌进后端既有的绑定表（`VarDecl::register_name`）。分配器
// 自己的判据由 src/ssa_regalloc.rs 的 13 条单测钉住；这里钉的是另外两件事：
// 判据在真实管线里成立（执行级夹具），以及**结果真的到达了产物**（§7.5 的教训：
// pass 接在管线里但产物逐字节相同，说明它根本没生效，而单测全绿看不出来）。

/// 分配判据的执行级对照：绑定与不绑定的变量放在同一份程序里，任何一条判据错了
/// 都会变成错值（期望值取 gcc 实测）。
#[test]
fn ssa_regalloc_locals_match_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-regalloc-locals",
        include_str!("../fixtures/opt/ssa_regalloc_locals.c"),
        "自动寄存器分配的某条准入判据失效（绑定变量读写错、或该拒的没拒）",
    );
}

/// 多出口循环（体内 return/break + 循环正常落出）四个档位都必须与 gcc 一致。
///
/// 这类形状上结构化重建只能表达一个落出口，政策是整函数退出 SSA 往返（判据与两端
/// 实测失败见 `ssa::has_multi_exit_cycle`）。夹具里每个期望值都取自 gcc 实测；初稿手算的
/// 3 处不符已被纠正（包括一个 break 先触发、return 永不可达的自相矛盾设计）。
///
/// 反向对照做过：把判据短期关掉后同一夹具在 -O2 报 4 条 FAIL（`n=1`/`n=2` 本该 1 却得 7，
/// 未命中时侧出口的语句仍执行），所以这条不是空跑。
#[test]
fn loop_multi_exit_matches_gcc_at_all_levels() {
    let source = include_str!("../fixtures/c/loop_multi_exit.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("loop-multi-exit", source, &[flag]),
            0,
            "{flag}: 多出口循环的重建与 gcc 不一致（循环出口接错线）"
        );
    }
}

/// 覆盖面反面：普通循环不得因为这条判据掉出 SSA。上一版判据用「续体被外层共享」
/// 作错锺，会把所有 for 循环挡掉（`for.end` 本来就同时是 preheader 假边的目标），
/// 当时就是靠 P12-B 这条验收测试变红才发现的。
#[test]
fn plain_loops_still_go_through_ssa() {
    let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i*3;return acc+n;}\n";
    let (on, _) = compile_asm_text_with_env("multiexit-plain", src, &["-O2"], &[("DCC_SSA_BAIL", "1")]);
    let (off, _) = compile_asm_text_with_env(
        "multiexit-plain-off",
        src,
        &["-O2"],
        &[("DCC_OPT_SKIP", "run_ssa_passes")],
    );
    assert!(!on.is_empty(), "编译应产出汇编");
    assert_ne!(on, off, "普通 for 循环仍应走 SSA 往返（否则判据过宽，覆盖面丢光）");
}

/// 零长/柔性数组成员（`z->data[i]`）四个档位都必须与 gcc 一致。
///
/// 这类下标需要「子对象地址」（对象地址 + 字段偏移），而 IR 只表达得了具名对象的取址，
/// 所以整函数退出 SSA 往返（判据与实测见 `ssa::Builder` 的 Index 分支注释：修前 -O2
/// 把 `data` 的**内容**当地址解引用，读出栈垃圾且每次不同）。
///
/// 反面：固定大小二维数组不得被这条判据误挡（`plain_loops_still_go_through_ssa` 同族的
/// 覆盖面守卫在 `subobject_array_decay_bails_while_2d_arrays_stay_in_ssa`）。
#[test]
fn vla_and_flexible_array_match_gcc_at_all_levels() {
    let source = include_str!("../fixtures/c/vla.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("vla-flexible", source, &[flag]),
            0,
            "{flag}: VLA / 零长数组 / 柔性数组成员与 gcc 不一致"
        );
    }
}

/// 「关掉 pass，产物必须不一样」——接入证明本身。
///
/// 为什么不能只比产物：`-O2` 下表达式的溅出池本来就会用 %rbx，所以「序言保存 %rbx」
/// 并不是绑定独有的特征（第一版断言就是这么写的，负向对照立刻假了）。真正的证据是
/// 两条一起成立：
///   1. 诊断里点名把累加器放进了寄存器（说明分配器算了并交给了后端）；
///   2. 关掉这条 pass 之后产物逐字节不同（说明它真的到达了产物，而不是算了没人用）。
#[test]
fn ssa_regalloc_reaches_the_product() {
    let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i*3;return acc+n;}\n";
    let (on, on_err) =
        compile_asm_text_with_env("regalloc-on", src, &["-O2"], &[("DCC_REGALLOC_DUMP", "1")]);
    let (off, off_err) = compile_asm_text_with_env(
        "regalloc-off",
        src,
        &["-O2"],
        &[("DCC_REGALLOC_DUMP", "1"), ("DCC_OPT_SKIP", "ssa_regalloc")],
    );
    // 具体发到哪个号由热度决定（`i` 比 `acc` 访问得多），所以这里只断言「两个热变量
    // 都进了寄存器，而且拿的是不同的号」—— 号的名字不是这条测试要保护的东西。
    assert!(
        on_err.contains("bind acc -> ") && on_err.contains("bind i -> "),
        "分配器没把循环里的两个热变量放进寄存器：\n{on_err}"
    );
    let regs: Vec<&str> = on_err
        .lines()
        .filter_map(|l| l.trim().strip_prefix("bind ").and_then(|r| r.split(" -> ").nth(1)))
        .collect();
    assert_eq!(
        regs.len(),
        regs.iter().collect::<std::collections::HashSet<_>>().len(),
        "同一个号被发了两次（诊断 {regs:?}）：\n{on_err}"
    );
    assert!(
        !off_err.contains("bind "),
        "关掉 pass 后诊断里仍然 bind ⇒ 开关没起作用，下面的产物比较就没意义了：\n{off_err}"
    );
    assert_ne!(
        on, off,
        "关掉 ssa_regalloc 后产物逐字节相同 ⇒ 这条 pass 没接到管线上（§7.5 的老坑）"
    );
}
