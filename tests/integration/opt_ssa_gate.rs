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
//   8. 内存左值的「读—改—写」必须在 IR 里真的读出又写回，且左值只求值一次。
//      `Expr::PostInc/PreInc` 的降级曾只认 `Expr::Ident` 操作数，其余形态直接返回常量
//      1（既不读也不写，也不标 `unsupported`）⇒ 实测 -O2 下 `int r = ++*p;` 返回 1 且
//      对象没被写；复合赋值那一半把 parser 展开出来的左值拷贝求值两遍 ⇒
//      `int r = a[i] += 7;` 返回未初始化垃圾、`a[i++] += 5` 写错格子。
//      判据与 codegen 共用 `crate::semantics`，修法是地址只求一次的读—改—写。
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

/// 内存左值读—改—写（`++*p` / `a[i]++` / `++s.f` / `a[i] += 7` / `*--p += 1` / volatile
/// / 循环体内）逐档与 gcc 对照。见文件头第 8 条与夹具注释里的实测数字。
#[test]
fn ssa_mem_rmw_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "ssa-mem-rmw",
        include_str!("../fixtures/opt/ssa_mem_rmw.c"),
        "SSA 降级丢掉了内存左值的读—改—写（返回常量且不写内存）",
    );
}

/// 多维下标的**写入**逐档与 gcc 对照。
///
/// 守的是两个已观测的失效，它们共用同一句夹具（`tests/fixtures/c/array2d_global_stride.c`）：
///   1. 行 stride 被按元素宽度算（§6 的「缺陷 6」）—— 行读回全相同；
///   2. `m[i][j] = v` 的内层基址 `m[i]` 是**数组类型**的左值，SSA 降级把它当右值取
///      （`IndexLoad{ty: Array}` ⇒ 重建后是个帧临时里的行副本）⇒ 写进了副本，数组本体
///      没被动。只在 -O3（展开后下标全成常量）可现，实测 `small7x3[3][0]`/`[6][0]`
///      读回 0 而同一行 `[i][1]`/`[i][2]` 正常。
///
/// 为什么不只靠语料级门：同一个夹具在 `c_cross.rs` 里只跑 `run_gcc_cross_check`
/// （相等于 -O0），能接住它的只有 `check_opt_equivalence.sh` 那条全档位语料级测试；
/// 而那是 611 夹具 × 3 档的十余分钟门，红一条时定位慢。这里按档逐个比，失败信息
/// 直接给出函数名与档位。完整历史与修法见 `docs/ssa-pass-upgrade-plan.md` §7.40。
#[test]
fn c_array2d_global_stride_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "c-array2d-global-stride-opt",
        include_str!("../fixtures/c/array2d_global_stride.c"),
        "多维下标写入在高档位下行副本化/行 stride 算错",
    );
}

/// P16 软件流水的执行级守门夹具（流水已默认接入 -O2/-O3）。
///
/// 曾经默认关闭的两条实测缺陷（kernel 内 store 实例越界写、链尾多累加一轮）同源于
/// 一个根因：SSA→AST 重建把同基名塌缩成一个槽，而模调度的 T 序模型不表达这种
/// 反依赖。现在由 `ssa::pipelining::emitted_reads_match` 在发射序上过一道判定，排不出
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

/// 逐字节对照汇编前摘掉 `.file` 行。
///
/// 为什么必须摘：`compile_asm_text_with_env` 把调用方给的 tag 写进源文件名，于是两次
/// 调用的产物**永远**在 `.file` 这一行上差一串字节。不摘的话 `assert_ne!` 会因为文件
/// 名而空跑（看起来绿，其实证明不了任何事），`assert_eq!` 会因为同一个原因无条件失败。
/// 实测这条不是理论问题：本轮新加的反向断言首跑就是被 `.file` 挡红的。
fn asm_body(asm: &str) -> String {
    asm.lines()
        .filter(|l| !l.starts_with("\t.file"))
        .collect::<Vec<_>>()
        .join("\n")
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
// `ssa::regalloc` 把热变量灌进后端既有的绑定表（`VarDecl::register_name`）。分配器
// 自己的判据由 src/ssa/regalloc.rs 的 13 条单测钉住；这里钉的是另外两件事：
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
    assert_ne!(
        asm_body(&on),
        asm_body(&off),
        "普通 for 循环仍应走 SSA 往返（否则判据过宽，覆盖面丢光）"
    );

    // 「体内 return + 循环落出」那一族（旧口径里的「多出口」）必须**真的在 SSA 里**：
    // 上面的普通循环只证明「没把好的挡光」，这一句证明「收窄判据不会偷偷退化成全挡回去
    // 也能绿」（挡回去产物照样与 gcc 一致，只有逐字节对照能区分）。
    //
    // 这一族曾因「EH 落地垫块被算进循环结构、被选成续体」而把 C++ 侧改坏（垫块 `goto`
    // 回入循环头 = CFG 上的一条回边）；现在由 lib 判据
    // `eh_pads_are_never_loop_members_or_continuations` 与 `cpp_abi_eh_group` 两面钉住。
    let early = "int a(int n){for(int i=0;i<n;i++){if(i==2)return 7;}return 1;}\n";
    let (e_ast, _) = compile_asm_text_with_env(
        "multiexit-early-ast",
        early,
        &["-O2"],
        &[("DCC_OPT_SKIP", "run_ssa_passes")],
    );
    let (e_ssa, _) = compile_asm_text_with_env("multiexit-early-ssa", early, &["-O2"], &[]);
    assert!(!e_ast.is_empty() && !e_ssa.is_empty(), "两侧都应产出汇编");
    assert_ne!(
        asm_body(&e_ast),
        asm_body(&e_ssa),
        "体内 return 的多出口循环应走 SSA 往返（本轮释放的覆盖面）"
    );

    // 反向的另一半：实测不可表达的那一族必须**确实掉出 SSA**（退回 AST 才谈得上
    // 「不优化但正确」）——旋转 do-while + 头块带具名写 + break 到续体。
    let bail = "int a(int n){int i=0,s=0;do{s+=i;i++;if(i==3)break;}while(i<n);return s+i;}\n";
    let (b_ast, _) = compile_asm_text_with_env(
        "multiexit-bail-ast",
        bail,
        &["-O2"],
        &[("DCC_OPT_SKIP", "run_ssa_passes")],
    );
    let (b_ssa, _) = compile_asm_text_with_env("multiexit-bail-ssa", bail, &["-O2"], &[]);
    // 实测读数：摘掉 `.file` 后两侧逐字节相同（-O2），即这个函数确实整函数掉出了 SSA；
    // 而上面 `early` 那一族摘掉 `.file` 后仍差 800 多行，确实在 SSA 里。
    assert_eq!(
        asm_body(&b_ast),
        asm_body(&b_ssa),
        "不可表达的旋转 do-while 必须整函数退回 SSA（否则又把错值放了出来）"
    );

    // 第三面（收窄的代价上限）：`while (k-- > 0)` 头块也带具名写，但它**没有**独立的 latch
    // 落出（头块自己就是循环条件的唯一落出点），不属于实测出错的那一族，不得被挡。
    // 这类倒退不会让任何 gcc 差分变红（AST 路径照样算对），只有产物逐字节对照看得见。
    let kdec = "int a(int n){int k=n,acc=0;while(k-- > 0){acc+=k;}return acc;}\n";
    let (k_ast, _) = compile_asm_text_with_env(
        "multiexit-kdec-ast",
        kdec,
        &["-O2"],
        &[("DCC_OPT_SKIP", "run_ssa_passes")],
    );
    let (k_ssa, _) = compile_asm_text_with_env("multiexit-kdec-ssa", kdec, &["-O2"], &[]);
    assert_ne!(
        asm_body(&k_ast),
        asm_body(&k_ssa),
        "`while (k-- > 0)` 被误挡出 SSA ⇒ 本轮收窄把最常见的循环一并丢了"
    );
}

/// 「循环出口所有权」模型的**放行集**逐形实测：体内 return / return+break / 多个 break
/// 合并 / continue-only / 旋转 do-while + 体内 return / 无限循环 / break 落到零语句桩块 /
/// 循环内 switch 的 case 出边 / 多前驱 return 块，四档都必须与 gcc 一致。
///
/// 为什么与下面那条对抗夹具分开：它们钉的是模型的两个相反方向 —— 这一条证明
/// 「放行的确实对」，下面那一条证明「测不透的已经挡回来」。历史两次（§7.28 / §7.32）
/// 都是只钉了一个方向就出事。
#[test]
fn loop_exit_ownership_shapes_match_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "loop-exit-ownership",
        include_str!("../fixtures/opt/loop_exit_ownership.c"),
        "出口所有权模型的放行形状在某一档与 gcc 不一致",
    );
}

/// 「循环出口所有权」模型的**边界集**：`goto` 出循环、break 落到带 phi 的汇聚块、
/// 双 latch、嵌套循环内外同时出边、旋转 do-while + 头块 break（本轮实测两种发法都错、
/// 现已整族挡回 AST）。
///
/// 这一条同时是挡下的**捕手**：把 `header_break_with_named_write` 去掉后
/// `do_break` 在 -O2/-O3 立即得 `2 2 5 9 …`（gcc `1 1 3 6 …`），所以它不是空跑。
#[test]
fn loop_exit_ownership_boundary_matches_gcc_at_all_levels() {
    assert_all_levels_match_gcc(
        "loop-exit-ownership-adv",
        include_str!("../fixtures/opt/loop_exit_ownership_adv.c"),
        "出口所有权模型的边界形状没挡住或误挡了",
    );
}

/// 零长/柔性数组成员（`z->data[i]`）四个档位都必须与 gcc 一致。
///
/// 这类下标需要「子对象地址」（对象地址 + 字段偏移）。IR 过去只表达得了具名对象的
/// 取址，于是把 `data` 的**内容**当地址解引用 —— 实测 -O2 读出栈垃圾而 -O0 正确；
/// 当时只能整函数退 SSA（§7.29）。现在由 `InstKind::MemberAddr` 真正表达，本测试从
/// 「不优化但正确」变成「优化后且正确」—— 这就是那条退路当初欠的账。
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

/// 子对象地址形态的全家福：`&p->b`（标量成员取址）、`z->data[i]`（零长数组）、
/// `f->data[i]`（柔性数组）、`&v[1]` 再下标。四个档位逐档与 gcc 对照。
///
/// 与上面那条分开是因为它们钉的是不同代码路径：`vla.c` 走完整 VLA 运行时链，
/// 本夹具只钉 `MemberAddr` 的降级/重建两侧（读写都要，只修一侧是历史上的重复错误）。
#[test]
fn subobject_member_addr_matches_gcc_at_all_levels() {
    let source = include_str!("../fixtures/c/subobject_addr.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("subobject-addr", source, &[flag]),
            0,
            "{flag}: 子对象地址（&成员 / 零长 / 柔性数组）与 gcc 不一致"
        );
    }
}

/// 指针指数组形参（`int (*pa)[N]`）的类型模型与下标形态：四个档位都必须与 gcc 一致。
///
/// 本条曾是 xfail，名字是 `pointer_to_array_index_is_a_known_baseline_defect`，按**实测
/// 矩阵**逐格钉「仍与 gcc 不一致」，并在断言文本里写明「哪一格变一致了就说明那一格被
/// 修好，请把它移到绿灯对照」。现在它按自己指的方向变成了绿灯。保留这段历史是因为
/// 根因被误归因过一次：
///
/// 真因不在 codegen 也不在 SSA，而在**形参声明符的结合顺序**：`int (*pa)[3]` 被建成
/// `Array(Ptr(Int),3)`（3 个 int* 的数组）而不是 `Ptr(Array(Int,3))`（指向「3 个 int
/// 的数组」的指针）。实测 `sizeof(pa)` 返 24（= 3*8）而 gcc 返 8。基址类型错，`pa[i][j]`
/// 的步长与 `(*pa)[i]` 的衰减必然全错 —— 所以它连 -O0 都是静默错值。当时我用「跳过
/// run_ssa_passes / 跳过各 AST pass / 回 HEAD」三步把它定成「基线 codegen 缺陷」，方向
/// 对了却没指向解析器；真正的捷径是最早该做的那一步：**用 sizeof 直接问类型**。
///
/// 下面的期望值都写在源程里并以返回 0 表示全部成立：这样「与 gcc 一致」与「算对了」
/// 同时被钉住（若只比 stdout 而不要求 0，我的期望写错时 gcc 也会配合地返回同一个
/// 非零值，那是一种假绿）。每个调用都走**语句序列**，不把多个修改同一数组的调用写进
/// 同一个表达式参数列表 —— 参数求值顺序在 C 里未规定，会产生与编译器无关的假
/// MISMATCH（本项目为此踩过一次）。
#[test]
fn pointer_to_array_param_matches_gcc_at_all_levels() {
    let src = "static int sizes(int (*pa)[3]) { return (int)sizeof(pa) * 10000 \
               + (int)sizeof(*pa) * 100 + (int)sizeof(pa[0]); }\n\
               static int sub(int (*pa)[3]) { pa[0][0] = 11; pa[1][2] = 22; \
               return pa[0][0] + pa[1][2]; }\n\
               static int dec(int (*pa)[3]) { (*pa)[2] = 7; \
               return (*pa)[2] + (*pa)[0]; }\n\
               int main(void) {\n\
                   int r1[6] = {0,1,2,3,4,5}; int r2[6] = {0,1,2,3,4,5};\n\
                   int r3[6] = {0,1,2,3,4,5}; int ok = 1;\n\
                   if (sizes((int (*)[3]) r1) != 81212) ok = 0;\n\
                   if (sub((int (*)[3]) r2) != 33) ok = 0;\n\
                   if (r2[0] != 11 || r2[5] != 22) ok = 0;\n\
                   if (dec((int (*)[3]) r3) != 7) ok = 0;\n\
                   if (r3[2] != 7) ok = 0;\n\
                   return ok ? 0 : 1;\n\
               }\n";
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("ptr-to-array-param", src, &[flag]),
            0,
            "{flag}: 指针指数组形参的类型模型 / pa[i][j] / (*pa)[i] 与 gcc 不一致"
        );
    }
}

/// 指针指数组的 **typedef** 形式：`typedef int (*arrp)[3];` 必须能编译，且
/// 尺寸、步长、读写都与 gcc 一致。四个档位逐档对照。
///
/// 与上面那条同一个声明符形状，但在不同的代码路径上：typedef 的 `(*name)` 分支
/// 只认函数指针（`)` 后必须是 `(`），所以 `typedef int (*arrp)[3];` 会在 gcc 能过的
/// 写法上连报 E0004/E0001/E0003 三条错。这一族是扫出来的：形参路径修好后，同类的
/// 结构体成员与局部变量路径实测已对（它们本来就是「先加后缀再包指针」），只有
/// typedef 这条路缺入分支。
///
/// `c[1][3] = 7` 必须落到 `buf[19]`（行步长 16），这才能证明类型是
/// Ptr(Array(char,16)) 而不是 Array(Ptr(char),16) —— 只看 sizeof 的话两者都是
/// 16*8/8 这类巧合相等的值，需要一次带步长的写才能分开。
#[test]
fn pointer_to_array_typedef_matches_gcc_at_all_levels() {
    let src = "typedef int (*arrp)[3];\ntypedef char (*rowp)[16];\n\
         int main(void) {\n\
         \x20   int r[6] = {0,1,2,3,4,5}; char buf[32]; int bad = 0;\n\
         \x20   arrp t = (arrp) r; rowp c = (rowp) buf;\n\
         \x20   if ((int)sizeof(arrp) != 8) bad |= 1;\n\
         \x20   if ((int)sizeof(t) != 8) bad |= 2;\n\
         \x20   if ((int)sizeof(*t) != 12) bad |= 4;\n\
         \x20   if (t[1][2] != 5) bad |= 8;\n\
         \x20   t[0][0] = 42; if (r[0] != 42) bad |= 16;\n\
         \x20   if ((int)sizeof(rowp) != 8) bad |= 32;\n\
         \x20   if ((int)sizeof(*c) != 16) bad |= 64;\n\
         \x20   c[1][3] = 7; if (buf[19] != 7) bad |= 128;\n\
         \x20   return bad;\n\
         }\n";
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("ptr-to-array-typedef", src, &[flag]),
            0,
            "{flag}: 指针指数组的 typedef（尺寸 / 步长 / 写入位置）与 gcc 不一致"
        );
    }
}

/// 一般的「左值地址」形态：`&a[i][j]`（下标基址本身是计算出来的数组行）。
///
/// 过去 `Expr::AddrOf` 降级遇到内层基址仍是数组时 `unsupported`，整函数退回 AST 通道
/// （正确但丢掉全部 SSA 优化）。现在由逐层 `InstKind::IndexAddr` 表达，且地址类临时值
/// 不被内联进取址操作数（否则重建出 `&(&m[i])[j]` 嵌套取址 —— 历史上从错值恶化成段错误）。
///
/// 夹具覆盖一维/二维/三维、非 2 次幂行步长、常量与变量下标混用、8 字节元素、经
/// `(*p)[j]` 的衰减基址、行指针再下标、全局二维数组。期望值取 gcc -O0 实测并按布局
/// 手算复核；四个档位逐档与 gcc 对照（错值与段错误都藏不住）。
#[test]
fn lvalue_addr_rows_match_gcc_at_all_levels() {
    let source = include_str!("../fixtures/c/lvalue_addr_rows.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("lvalue-addr-rows", source, &[flag]),
            0,
            "{flag}: 一般左值地址（&a[i][j] 逐层 IndexAddr）与 gcc 不一致"
        );
    }
}

/// 只在**链接期**才暴露的两个 -O2 缺陷（COMPILE-FAIL 逐条定性的产出）：
///
///   1. 嵌套函数的定义在 SSA 往返后消失（`-O2` 报 `undefined reference to 'id'`）；
///   2. `__attribute__((copy(fn)))` 的**目标**被跨函数 DCE 吃掉，留下一条悬空 `.set`
///      （报的是「undefined reference to public_api」，完全看不出被删的是 `_internal_impl`）。
///
/// 为什么必须在这里显式跑**链接**：Tier 0 全部只 `-c`，上面两个形状在 `-c` 下全绿；
/// 而 OPT 等价门禁会跑链接，所以下一次改动弄坏它们时，只有这条测试能在 CI 里
/// 指到具体形状（而不是一个变红的门禁计数）。
#[test]
fn link_only_defects_survive_all_levels() {
    let nested = "int main(void){ int id(void){ return 3; } return id() == 3 ? 0 : 1; }\n";
    let copy_alias = "static int _internal_impl(int a, int b){ return a + b; }\n\
                      int public_api(int a, int b) __attribute__((copy(\"_internal_impl\")));\n\
                      int main(void){ return public_api(3, 4) == 7 ? 0 : 1; }\n";
    for (tag, src) in [("nested-func-link", nested), ("copy-alias-link", copy_alias)] {
        for flag in ["-O0", "-O1", "-O2", "-O3"] {
            let (build, exe) = run_compile_and_run_flags(tag, src, &[flag, "-std=gnu2x"]);
            assert!(
                build.status.success(),
                "{tag} [{flag}] 链接失败: {}",
                String::from_utf8_lossy(&build.stderr)
            );
            assert_eq!(run_exe(&exe), 0, "{tag} [{flag}] 运行结果不对");
            let _ = std::fs::remove_file(exe);
        }
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

// ===== G2：AST 层 `eliminate_unreachable` (`dce_block`) 与 Label/Return 混排 =====
//
// 缺陷代号 G2（docs/wp_multi_exit_loop.md §4 移交）。旧 `dce_block` 在顶层 `Return`
// 之后 stopped=true，只保留后续 Label —— 但 `Stmt::Label { stmt }` 只把紧随标签的
// **一条语句**纳入自己，标签之后到下一个 Label/terminator 之间的顶层 item（典型
// 形态是 `out: <body>; return s;` 里的 `return s;`）仍是「跳到 out 之后可达」的路径，
// 被误清 ⇒ `goto out` 从 `out:` 顺落到 `late:` 再从函数尾 fall-off，`%eax` 只剩
// 最后一条对 `s` 的写，gcc `-3` 变 dcc `-8`。修法：遇到 Label 就把 stopped 复位。

/// G2 最小形状（双标签 + 顶层 Return + goto）与循环+双标签两条同族形状，
/// 四档逐个与 gcc 对照。失败信息直接给出档位，方便下次定位是 `-O2` 还是 `-O3`
/// 一侧回归。夹具内 F3（`return` 后无标签的真不可达）用来钉「修 G2 不得把 dce
/// 修成完全不删」。
#[test]
fn dce_label_after_return_minimal_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/dce_label_after_return_minimal.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("dce-label-after-return-min", src, &[flag]),
            0,
            "{flag}: dce_block 把 Label 后顶层 Return 当不可达清掉 ⇒ \
             goto out 直落进 late 再从函数尾 fall-off（F1/F2 形状）",
        );
    }
}

/// G2 在「循环出口所有权」侧的原始夹具：`goto` 跳到循环之后的第二个标签。
/// 从前因值错故意没进门禁，见 docs/wp_multi_exit_loop.md §4；G2 修好后同一
/// 夹具直接作为回归门禁。四档逐个比，失败时给出档位与 `goto_out2` 函数名。
#[test]
fn loop_exit_ownership_goto_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/loop_exit_ownership_goto_pending.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("loop-exit-ownership-goto", src, &[flag]),
            0,
            "{flag}: a_goto_out2 在 `out:` 与 `late:` 双标签处被 dce_block 误清 \
             → gcc `7 7 -3 -3 -3 -3 -3` 应变为 `7 7 -8 -8 -8 -8 -8`",
        );
    }
}
