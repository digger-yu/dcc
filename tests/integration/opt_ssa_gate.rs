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
//   9. 第 8 条那台读—改—写机器不能用在**位域**左值上：位域没有自己的存储地址，
//      `&obj.bf` 只能给出容器（`StorageUnit`）的地址，于是整容器被当成一个值加 1 ⇒
//      进位溢出到相邻位域，且「结果归约到位域类型」整个丢失。实测 -O2/-O3 下
//      `struct {unsigned b:3; unsigned pad:29;} v; v.b=7; v.pad=5; v.b+=1;` 得
//      `b=0 pad=6`（gcc 是 `pad=5`），gcc-torture pr55750 的 `arr[i].n++` 在 -O2 abort。
//      `bitfield_inc/expected.s` 那份快照当年是从这条误编译的输出冻结的，所以反汇编
//      oracle 一直是绿的 —— 判据只能建在运行期与 gcc 的对拍上。
//
//   10. 写侧的「赋值表达式的值」必须有指令真的产出它。`lower_lvalue`（`src/ssa/lower.rs`）
//       把 `*p = v` / `a[i] = v` / `p->f = v` 降成一条写指令（`dest` 为空），然后
//       `b.fresh("store"|"idxstore"|"memstore", ty)` 造一个临时变量当**整个赋值表达式的值**
//       返回 —— 而没有任何指令写过它。重建阶段为它分配栈槽、只读不写 ⇒ -O2/-O3 读回栈垃圾：
//       实测 `long L(void){ ui[0]=0; vv=0x100000005ULL; return (ui[0] = vv); }` gcc 给 5、
//       dcc 给 317169664；25 个形态（解引用/下标/多维/成员点与箭头/联合体/位域/窄类型/
//       浮点转整数/链式赋值/作实参）在 -O2 与 -O3 全错，-O0/-O1（AST 通道）全对。
//       C11 6.5.16p3 要的正是「左值转换后的左操作数」，所以修法是写完紧跟一条同址同类型的
//       载入，用它的 `dest` 当结果值。
//       同一条夹具还守住 codegen 侧的一处字节序：`movss/movsd` 的 REX.B 必须排在 legacy
//       前缀（F2/F3）**之后**，前缀之后 REX 才紧邻操作码；反着发时 REX 作废、基址从 %r8
//       退化回 %rax，实测 `return r + (int)l.d;` 逐档段错误。
//
//   11. -O3 的循环分段（P10 strip mining, `src/loop_opt.rs`）不得重排循环体内**可观察
//       副作用的执行顺序**。旧形态是 ts×ts 的 blocking（`ti/tj/i/j` 四层）：它与原循环
//       「逐点同集合同重数」但按块重排，而本仓没有依赖分析（PDO）⇒ 体内有 printf 时就是
//       静默误编译（实测 -O3 与 gcc 打印的数字集合相同、顺序不同）。同一 pass 还有一条
//       `break` 闸门：分段把内层拆成「块计数器 + 子循环」两层，体内未被屏蔽的 `break`
//       因此多跑迭代（实测去掉闸门后 12 变 18）。
//
//   12. 混合符号整型比较的**公共类型**必须按 C11 6.3.1.8 判「有符号侧能否表示无符号侧
//      全部值」。`Type::usual_arith_type` 曾只看秩，于是 LP64 上同为 64 位的
//      `unsigned long` × `long long` 停在有符号侧；而 `dispatch.rs` 的 Binop 臂当时**另抄
//      了一份**只看秩的秩表，所以两处得一起改回单一真源。这条与档位无关（-O0 就错），
//      拿 -O0 当基准的跨档对拍看不见它，只有与 gcc 对拍能看见 —— 见夹具。
//
//   13. SSA 的整型常量是**无类型**的（`Operand::Int(i128)`），重建时按量级阶梯猜类型 ⇒
//      `0xffffffffUL` 被猜成 `long long`，字面量后缀的无符号性在半路丢失。实测形态是
//      把值先存进变量再比较（SCCP 把两侧都折成常量后只剩 `Operand::Int`），只有跑完
//      SSA 通道的档位会错。现在 build 期把前端写下的字面量类型记进
//      `Function::const_int_tys`，重建优先读它（歧义才回落阶梯）。
//
//   14. 常量折叠器 `const_eval::cmp_binop_values` 曾自拍一套符号性判据：「任一操作数自身
//      无符号 ⇒ 按无符号比较」（应是**公共类型**），且它写的 `f(a as u128 as i128, ...)`
//      在 Rust 里是**往返恒等**（负数原样带回），「重解释为无符号」完全没生效 ⇒
//      `-1 > 0xffffffffUL` 被折成 0（gcc 给 1）。折叠在 **-O0 也会跑**（`function_inline`
//      排在 `level == 0` 提前返回之前，`inline_expr` 末尾无条件调 `fold_expr`），
//      故这条四档全错。两个错向必须同修：只看「任一侧无符号」会把 `-1L > 1U`
//      （公共类型 `long`，应 0）判成无符号，旧写法只是被往返恒等恰好抵消。
//
//   15. 「按值」记账盖不住小常量：`1` 既是 `int` 也是 `unsigned int` ⇒ 歧义 ⇒ 回落阶梯
//      ⇒ `int`。于是 `int x=-1; unsigned int y=1; return x>y;` 被 SCCP 把**带类型的**
//      `Var(y.1: UInt)` 换成**无类型的** `Int(1)` 后，重建只能猜回 `int` ⇒ 有符号条件码
//      （实测 -O2/-O3 得 0，应 1；`DCC_SSA_OFF=sccp` 时 IR 仍保留两个带类型 Var，就正确）。
//      现在比较指令的两个操作数类型在 build 期按 `dest` 名记进 `Function::cmp_operand_tys`，
//      重建时用它给「裸 `Expr::Int` 操作数」补回类型。
//
// 全部通过 `run_gcc_cross_check_opt` 运行：带 `-O` 档位、带超时、与 gcc 逐字节对照。
// 共用的逐档对照助手 `assert_all_levels_match_gcc`、汇编归一助手 `asm_body` 与
// 开关对照助手 `dcc_asm_with_pipe` 都住在 `tests/common/harness.rs`。

#[test]
fn ssa_gate_switch_loop_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-gate-switch-loop",
        include_str!("../fixtures/opt/ssa_gate_switch_loop.c"),
        "switch/label 里包的循环被 SSA 闸门漏判 ⇒ rebuild_cycle 出错",
    );
}

#[test]
fn ssa_order_multi_assign_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-order-multi-assign",
        include_str!("../fixtures/opt/ssa_order_multi_assign.c"),
        "块内重排颠倒了同一变量的多次赋值顺序",
    );
}

#[test]
fn ssa_loop_shapes_match_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-loop-shapes",
        include_str!("../fixtures/opt/ssa_loop_shapes.c"),
        "循环形态在优化档位下行为不一致",
    );
}

#[test]
fn ssa_rebuild_inline_order_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-rebuild-inline-order",
        include_str!("../fixtures/opt/ssa_rebuild_inline_order.c"),
        "重建期内联把临时值的读搬到了同基名写入之后 (或丢弃了节点头的写)",
    );
}

#[test]
fn ssa_addr_taken_local_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-addr-taken-local",
        include_str!("../fixtures/opt/ssa_addr_taken_local.c"),
        "被取址的标量局部被 SSA 版本化, 丢了指针的间接写",
    );
}

#[test]
fn ssa_unroll_nested_iv_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-unroll-nested-iv",
        include_str!("../fixtures/opt/ssa_unroll_nested_iv.c"),
        "-O3 展开没替换嵌套语句里的归纳变量, 或复制了悬空的 break/continue",
    );
}

#[test]
fn gvn_loop_copied_iv_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "gvn-loop-copied-iv",
        include_str!("../fixtures/opt/gvn_loop_copied_iv.c"),
        "AST 层 GVN 用块内写入集合判定, 删掉了循环变量的拷贝声明",
    );
}

#[test]
fn underscore_locals_match_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "underscore-locals",
        include_str!("../fixtures/opt/underscore_locals.c"),
        "下划线命名的用户变量被当成合成临时值, 或 SSA 版本名与临时值 base 撞名",
    );
}

/// 内存左值读—改—写（`++*p` / `a[i]++` / `++s.f` / `a[i] += 7` / `*--p += 1` / volatile
/// / 循环体内）逐档与 gcc 对照。见文件头第 8 条与夹具注释里的实测数字。
#[test]
fn ssa_mem_rmw_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-mem-rmw",
        include_str!("../fixtures/opt/ssa_mem_rmw.c"),
        "SSA 降级丢掉了内存左值的读—改—写（返回常量且不写内存）",
    );
}

/// 位域左值的读—改—写逐档与 gcc 对照（见文件头第 9 条）。
///
/// 这条之所以独立成一条：`bitfield_inc/expected.s` 那份反汇编快照曾被从**误编译输出**
/// 冻结下来（容器整宽度加 1），文本层 oracle 从此再也抓不到它。运行期与 gcc 对拍才
/// 是唯一能立住判据的地方。
#[test]
fn ssa_bitfield_rmw_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-bitfield-rmw",
        include_str!("../fixtures/opt/ssa_bitfield_rmw.c"),
        "位域左值被按容器宽度做读—改—写（进位污染相邻位域、结果未归约到位域类型）",
    );
}

/// 赋值表达式的**值**（C11 6.5.16p3：取左操作数左值转换后的值）逐档与 gcc 对照。
///
/// 守的是两个已观测的失效：
///   1. `lower_lvalue` 的三个 store 分支只发 store、不发任何产值的指令，返回的
///      `fresh()` 临时从未被 define ⇒ 重建后读**未初始化栈槽**。-O2/-O3 实测
///      `return (ui[0]=vv)` 得到 317169664，gcc 给 5；25 个写侧形状全错。
///   2. `movss/movsd` 的 REX.B 排在了 legacy 前缀（F2/F3）**之后**——前缀顺序是
///      硬性编码规则，REX 必须紧邻操作码，否则 `41 F2 0F 10 00` 解码成
///      `movsd (%rax),%xmm0`（REX 被丢弃），基址寄存器读错，各档位一律段错误。
#[test]
fn ssa_assign_value_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-assign-value",
        include_str!("../fixtures/opt/ssa_assign_value.c"),
        "写侧赋值表达式的值没有指令产出（-O2/-O3 读未初始化栈槽）；movss/movsd 的 REX.B 排在 legacy 前缀之后",
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
/// 直接给出函数名与档位。
#[test]
fn c_array2d_global_stride_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
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
    harness::assert_all_levels_match_gcc(
        "ssa-pipe-reduction",
        include_str!("../fixtures/opt/ssa_pipe_reduction.c"),
        "P16 落地的 peel/kernel/轮转 phi 改变了归约语义",
    );
}

#[test]
fn ssa_pipe_nested_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-pipe-nested",
        include_str!("../fixtures/opt/ssa_pipe_nested.c"),
        "内层循环被流水时外层携带值丢失或外层推进链被破坏（曾表现为死循环）",
    );
}

/// 三种流水判定并存的样本：该流水的（两条 6 级依赖链）与不该流水的（load 归约
/// depth=0、5 级链 depth=0、trip 太短）放在同一个翻译单元里，默认路径逐档与 gcc 对照。
#[test]
fn ssa_pipe_chain_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "ssa-pipe-chain",
        include_str!("../fixtures/opt/ssa_pipe_chain.c"),
        "流水落地或准入判定改变了行为",
    );
}

/// 「pass 已接入」必须由**产物差异**证明，不能由代码已调用证明 —— 否则它可以是一个
/// 永远不生效的默认参数，而上面所有夹具与全部门禁依旧全绿（§7.5 就是这么发现的：
/// 接入点在管线末尾时，开/关的 -O2 汇编 sha256 完全相同）。
#[test]
fn ssa_pipe_default_on_changes_the_product() {
    let src = include_str!("../fixtures/opt/ssa_pipe_chain.c");
    let on = harness::dcc_asm_with_pipe("pipe-chain", src, None);
    let off = harness::dcc_asm_with_pipe("pipe-chain", src, Some("pipe"));
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
    harness::assert_all_levels_match_gcc(
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
        harness::asm_body(&on),
        harness::asm_body(&off),
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
        harness::asm_body(&e_ast),
        harness::asm_body(&e_ssa),
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
        harness::asm_body(&b_ast),
        harness::asm_body(&b_ssa),
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
        harness::asm_body(&k_ast),
        harness::asm_body(&k_ssa),
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
    harness::assert_all_levels_match_gcc(
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
    harness::assert_all_levels_match_gcc(
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

/// Phase 5.2c 的**前提核对**：带异常落地垫的函数今天拿不到自动分配的寄存器家，而挡住它的
/// 不是任何一条 EH 判据，是两道结构性闸：
///   1. C++ 前端把函数体降到 AST 时不调用分配器（`cpp/native.rs` 直接 `ssa::rebuild_block`）；
///   2. 落地垫形态在 AST 里必然带着地垫标签与回入标签，而带 `Stmt::Label` 的函数被
///      `ssa::structure::is_structured` 判为非结构化 ⇒ `build_from_func` 返 `None` ⇒ 整个
///      函数不进 SSA，也就不会被规划（所以闸 1 被绕过时闸 2 仍在）。
///
/// 于是设计 §7 5.2 要的「落地垫的寄存器约定」今天**没有消费者**：没有任何带 EH 的函数会被
/// 绑号。我没有为此在分配器里加一条 EH 判据（那是到不了的入口，与 5.2a 同一个决定），而是
/// 把前提钉住：同一个 TU 里非 EH 的热函数必须真的被规划（反向对照，保证不是空跑），三个带
/// EH 的函数一行 dump 都不许出现。前提一变就红，那时必须先补上落地垫的判据：垫入口只毁
/// `%rax`/`%rdx`（其他 callee-saved 跟异常边保持），而对象/selector 槽必须是**具名局部**
/// （`cpp/lower_ir.rs` 已经要求这一点，因为 SSA 临时会被内联掉）。
/// EH 的**行为**正确性由 `cpp_abi_eh_group` 那套与 g++ 的交叉对照负责，本用例不重复。
#[test]
#[cfg(unix)]
fn eh_landing_pad_functions_are_never_auto_bound() {
    let src = "extern \"C\" int printf(const char *, ...);\n\
               int hot(long n){ long a=0,b=0,c=0,d=0,e=0,f=0;\n\
               for(long i=0;i<n;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}\n\
               return (int)(a+b+c+d+e+f); }\n\
               int with_eh(long n){ long a=0,b=0,c=0,d=0,e=0,f=0;\n\
               try { for(long i=0;i<n;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e; if(a==3) throw 1;} }\n\
               catch(int x){ f+=x; } return (int)(a+b+c+d+e+f); }\n\
               int eh_no_loop(long n){ long a=n+1,b=a*2,c=b+a,d=c*3,e=d+c,f=e*2;\n\
               try { if(a>3) throw 2; } catch(int x){ f+=x; } return (int)(a+b+c+d+e+f); }\n\
               int cleanup_only(long n){ long a=n+1,b=a*2,c=b+a,d=c*3,e=d+c,f=e*2;\n\
               try { if(a>3) throw 3; } catch(...){ f+=7; } return (int)(a+b+c+d+e+f); }\n\
               int main(){ printf(\"%d %d %d %d\\n\", hot(5), with_eh(5), eh_no_loop(5), cleanup_only(5));\n\
               return 0; }\n";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let cpath = dir.join(format!("dcc-eh-nohome-{pid}.cpp"));
    let opath = dir.join(format!("dcc-eh-nohome-{pid}.o"));
    fs::write(&cpath, src).expect("write cpp source");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .env("DCC_REGALLOC_DUMP", "1")
        .args(["-O2", "-c", "-o"])
        .arg(&opath)
        .arg(&cpath)
        .output()
        .expect("run dcc -O2 -c");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_file(&cpath);
    let _ = fs::remove_file(&opath);
    assert!(out.status.success(), "-O2 应能编过这份 C++ 源：\n{stderr}");
    assert!(
        stderr.contains("[regalloc] hot"),
        "反向对照：同一个 TU 里不带 EH 的热函数必须真的被规划，否则下面那条断言没有判定力：\n{stderr}"
    );
    for name in ["with_eh", "eh_no_loop", "cleanup_only"] {
        assert!(
            !stderr.contains(&format!("[regalloc] {name}")),
            "{name} 带落地垫却被规划了：落地垫的寄存器约定还没进干涉模型，先补判据再放开 \n（见设计 §7 5.2 与 §11.13 的 N2/N5）"
        );
    }
}

/// Phase 5.2b 的同类前提核对：**带嵌套函数的函数拿不到自动分配的寄存器家**。
///
/// §7 5.2 列的第二条干涉对象是「嵌套函数按帧偏移访问外层变量」那条路（P12-A 的准入条件 4，
/// 即今天 `plan_for` 里的 `nested-function-in-scope`）。本轮实测发现两件事，所以这一步**没有**
/// 去收窄那条规则：
///   1. 那条规则在真实管线上命中 **0** —— 含 `BlockItem::Decl(Decl::Func)` 的函数在进 SSA 前
///      就被 `opt.rs::block_has_unsafe_ssa_pattern` 整函数踢出管线，所以把它收成「只拒被
///      捕获者」在语料上多不出一个绑定（本用例就是这条事实的常驻版本）；
///   2. 真要拿到收益，得先修好另一族缺陷（并行 codegen 不回收嵌套函数符号、`collect_upvars`
///      漏 VLA 声明维度与 EH 子树、逸出判据两套不分叉），设计 §11.13 的 N1/N5/N8。
/// 于是这里只钉前提：同 TU 里不带嵌套的热函数必须真的被规划（反向对照），带嵌套函数的那个
/// 函数一行 dump 都不许出现。前提一变就红，那时再按 §11.13 清完障碍、并把拒绑集换成
/// 「直接子 `FuncDecl.upvars` 的名字并集」（而不是继续用整函数开关）。
#[test]
#[cfg(unix)]
fn nested_function_bodies_are_never_auto_bound() {
    let src = "long hot(long n){ long a=0,b=0,c=0,d=0,e=0,f=0;\n\
               for(long i=0;i<n;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}\n\
               return a+b+c+d+e+f; }\n\
               long with_nested(long n){ long id(void){ return 3; } return id() + n; }\n\
               int main(void){ return (int)((hot(4) + with_nested(7)) % 3); }\n";
    let (asm_text, stderr) =
        compile_asm_text_with_env("nested-nohome", src, &["-O2"], &[("DCC_REGALLOC_DUMP", "1")]);
    assert!(!asm_text.is_empty(), "应产出汇编：\n{stderr}");
    assert!(
        stderr.contains("[regalloc] hot"),
        "反向对照：同一个 TU 里不带嵌套函数的热函数必须真的被规划：\n{stderr}"
    );
    assert!(
        !stderr.contains("[regalloc] with_nested"),
        "带嵌套函数的函数却被规划了：而它的外层变量可由嵌套体按帧偏移读，窄判据还没建起来 \n（先按设计 §11.13 清完 N1/N5/N8）：\n{stderr}"
    );
}

/// `-fno-omit-frame-pointer` 的两条实测判据（设计 §11.13 的 N2/N3，本轮修）。
///
/// 为什么这条用例必须存在：仓里**从来没有** FP 档的测试（全量用例与9 个测试用例脚本
/// 都是默认档），所以这两个缺陷能存活：一是一个调用点差 8 字节导致 libc 浮点路径
/// `movaps` 当场崩（实测 SIGSEGV/139，不带该 flag 与 gcc 都正常），二是 FDE 把 r13 与 rbp
/// 写成同一个槽（两条都是 `at cfa-16`），展开穿过 FP 帧时把帧基址恢复成 rbp 的值。
#[test]
#[cfg(unix)]
fn frame_pointer_mode_keeps_calls_aligned_and_fde_distinct() {
    let source = include_str!("../fixtures/opt/frame_pointer_aligned_calls.c");
    // 1) 运行：四档逐档与 gcc 对照（139=SIGSEGV 就是对齐未修的形态）
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("fp-aligned", source, &[flag, "-fno-omit-frame-pointer"]),
            0,
            "{flag} -fno-omit-frame-pointer: 调用点或读数不对"
        );
    }
    // 2) 产物：FP 序言在 `mov %rsp, %rbp` 之后紧跟一次 `sub $8, %rsp`（对齐补偿）
    let (asm_text, stderr) = compile_asm_text_with_env(
        "fp-prologue",
        source,
        &["-O0", "-fno-omit-frame-pointer"],
        &[],
    );
    assert!(!asm_text.is_empty(), "应产出汇编：\n{stderr}");
    let anchor = "mov %rsp, %rbp";
    let at = asm_text
        .find(anchor)
        .unwrap_or_else(|| panic!("FP 档序言里没有 `{anchor}`：\n{asm_text}"));
    assert!(
        asm_text[at + anchor.len()..].trim_start().starts_with("sub $8, %rsp"),
        "FP 档序言必须紧接一次 `sub $8, %rsp` 把体内对齐补回来：\n{}",
        asm_text[at..].chars().take(120).collect::<String>()
    );
    // 3) FDE：r13 在 cfa-24、rbp 在 cfa-16（两者不得同址）
    let (obj_out, obj) =
        run_object_test_flags("fp-fde", source, &["-O0", "-fno-omit-frame-pointer"]);
    assert!(
        obj_out.status.success(),
        "FP 档应能编过：{}",
        String::from_utf8_lossy(&obj_out.stderr)
    );
    let dump = Command::new("readelf")
        .args(["--debug-dump=frames"])
        .arg(&obj)
        .output()
        .expect("readelf");
    let _ = std::fs::remove_file(&obj);
    let text = String::from_utf8_lossy(&dump.stdout).to_string();
    assert!(
        text.contains("DW_CFA_offset: r13 (r13) at cfa-24"),
        "FP 档的 r13 保存槽应是 CFA-24（因子 3），否则与 rbp 同址：\n{text}"
    );
    assert!(
        text.contains("DW_CFA_offset: r6 (rbp) at cfa-16"),
        "rbp 保存槽应在 CFA-16：\n{text}"
    );
    assert!(
        !text.contains("DW_CFA_offset: r13 (r13) at cfa-16"),
        "两条 DW_CFA_offset 不得指向同一个地址：\n{text}"
    );
    // 4) 反向对照：默认档不受这条影响（既不出现 FP 序言，也不多推 8 字节）
    let (def_text, _) = compile_asm_text_with_env("fp-default", source, &["-O0"], &[]);
    assert!(
        !def_text.contains("mov %rsp, %rbp"),
        "默认档不应出现 FP 序言：\n{def_text}"
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

/// N1：嵌套函数的**程序级符号事实**必须随并行 chunk 带回主 Codegen。
///
/// 三条断言各钉一半，缺一不可：
///   1) 四档逐档与 gcc 对照 —— 符号丢了运行期就是链接失败，只编 `-S` 看不见；
///   2) 并行与串行的 `.text` 反汇编逐字节相同，且六个唯一名**后缀编号相同** ——
///      钉住「计数不能在 worker 里各起一路」（旧实测并行 `ib_add_b.0` / 串行
///      `ib_add_b.1`，两份产物因此永远对不上）；
///   3) `.symtab` 里每个嵌套函数恰好一条 LOCAL/FUNC 定义项、零条 UND ——
///      旧实测 `-O2` 全是 `U ia_add_a.0`，而重复发射那一半让同一符号进了两遍。
///
/// 为什么不比整个 `.o` 字节：`-O2` 并行下每个 worker 都对自己的 `string_pool`
/// 快照去重，跨 worker 重复的字符串会被各存一份（`.rodata` 膨胀，见 §11.13 的
/// N10），那是另一条独立立项的债，不能拿它当本用例的判据。
#[test]
#[cfg(unix)]
fn nested_func_symbol_facts_survive_parallel_codegen() {
    let source = include_str!("../fixtures/opt/nested_func_symbol_facts.c");
    // 1) 四档逐档与 gcc 对照
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("nested-sym-facts", source, &[flag]),
            0,
            "{flag}: 嵌套函数（含两层嵌套与 &nested 取址）的运行期结果与 gcc 不同"
        );
    }
    // 2) 并行 vs 串行的 .o：符号与代码必须同源
    let src = std::env::temp_dir().join(format!("dcc-nested-sym-{}.c", std::process::id()));
    fs::write(&src, source).expect("write nested fixture");
    let compile = |serial: bool| -> std::path::PathBuf {
        let obj = std::env::temp_dir().join(format!(
            "dcc-nested-sym-{}-{}.o",
            std::process::id(),
            if serial { "ser" } else { "par" }
        ));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
        if serial {
            cmd.env("DCC_NO_PARALLEL", "1");
        } else {
            cmd.env_remove("DCC_NO_PARALLEL");
        }
        let out = cmd
            .args(["-O2", "-c", "-o"])
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("run dcc");
        assert!(
            out.status.success(),
            "{} 编译失败: {}",
            if serial { "串行" } else { "并行" },
            String::from_utf8_lossy(&out.stderr)
        );
        obj
    };
    let par = compile(false);
    let ser = compile(true);
    let disasm = |p: &std::path::Path| -> String {
        let d = Command::new("objdump")
            .args(["-d"])
            .arg(p)
            .output()
            .expect("objdump");
        assert!(d.status.success(), "objdump -d 失败: {p:?}");
        // 去掉头行：它带的是对象文件名，两侧文件名本来就不同（不依赖 locale）
        let stem = p.file_name().unwrap().to_string_lossy().to_string();
        String::from_utf8_lossy(&d.stdout)
            .lines()
            .filter(|l| !l.contains(&stem))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (par_asm, ser_asm) = (disasm(&par), disasm(&ser));
    if par_asm != ser_asm {
        // 失败信息直接给出第一条不同的反汇编行，免得下次再靠肉眼比两坨字节
        let pd: Vec<&str> = par_asm.lines().collect();
        let sd: Vec<&str> = ser_asm.lines().collect();
        let first = (0..pd.len().max(sd.len()))
            .find(|i| pd.get(*i) != sd.get(*i))
            .unwrap_or(0);
        panic!(
            "并行与串行的 .text 不同 ⇒ worker 侧还在自己分配嵌套函数的唯一名\n\
             第一条差异在第 {} 行:\n  par: {:?}\n  ser: {:?}",
            first,
            pd.get(first).unwrap_or(&"<无>"),
            sd.get(first).unwrap_or(&"<无>")
        );
    }
    let par_syms = elf64_syms(&fs::read(&par).expect("read parallel .o"));
    let ser_syms = elf64_syms(&fs::read(&ser).expect("read serial .o"));
    // N11：同一份源连编两次必须字节相同。旧实测 `emit_trampoline_to_stack` 按
    // HashMap 随机序发射多个 trampoline 的写入块 ⇒ 两个 `lea -768/-800(%r13)`
    // 会互换（8 连编里 4 次对 4 次），而语料里没有含嵌套函数的夹具，
    // 所有一直跑绿的「可复现」检查从来推不到这条。
    let par2 = compile(false);
    let par2_asm = disasm(&par2);
    let par_bytes = fs::read(&par).expect("read parallel .o");
    let par2_bytes = fs::read(&par2).expect("read second parallel .o");
    let _ = std::fs::remove_file(&par2);
    assert_eq!(
        par_asm, par2_asm,
        "同一份源两次发射的 .text 不同 ⇒ 还有按 HashMap 随机序发射的站点"
    );
    assert_eq!(
        par_bytes, par2_bytes,
        "同一份源两次 .o 不同 ⇒ 除 .text 之外的产物事实也不确定"
    );
    let _ = std::fs::remove_file(&par);
    let _ = std::fs::remove_file(&ser);
    let _ = std::fs::remove_file(&src);
    // 六个嵌套函数：唯一名与编号必须逐个对上
    let want = [
        "ia_add_a.0",
        "ib_add_b.1",
        "ic_add_b.2",
        "mid_deep.3",
        "leaf_mid_deep.3.4",
        "nb_use_addr.5",
    ];
    for name in want {
        let hits: Vec<_> = par_syms.iter().filter(|s| s.name == name).collect();
        assert_eq!(
            hits.len(),
            1,
            "并行 .symtab 里 `{name}` 应有且仅有一条定义项，实际 {} 条: {:?}",
            hits.len(),
            par_syms.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(hits[0].bind, 0, "`{name}` 必须是 LOCAL 绑定（嵌套函数）");
        assert_eq!(hits[0].stype, 2, "`{name}` 必须是 STT_FUNC");
        assert_eq!(hits[0].shndx, 1, "`{name}` 必须定义在 .text（0 = UND）");
        assert!(hits[0].size > 0, "嵌套函数的 st_size 必须随 chunk 带回: {name}");
        // 串行侧同一名字、同一编号
        assert!(
            ser_syms.iter().any(|s| s.name == name && s.shndx == 1),
            "串行侧应定义同一个 `{name}`（编号不一致就是两套真源）"
        );
    }
    // 没有一条 UND 的同名残留（旧形状就是发成 UND 后链接失败）
    for name in want {
        assert!(
            !par_syms.iter().any(|s| s.name == name && s.shndx == 0),
            "`{name}` 不得同时存在 UND 项"
        );
    }
}

/// VLA/VM 对象的**运行时尺寸身份**必须在四个档位指向同一个对象。
///
/// 这条是 `vla_and_flexible_array_match_gcc_at_all_levels` 照不出来的那一半：那份夹具
/// 只「写完再读回」，而基址错的形状是**读写两侧同错**，因此自洽。本夹具把
/// `sizeof a`（读元数据槽里的 size 字段）、元素写（落回同一区域）与
/// `probe(1, a)`（调用侧要读槽内容当数据区基址）放在同一份源里，错值才现形。
///
/// 两个档位口径都要钉：`-O0/-O1` 走 AST 直发（消费侧基址由
/// `semantics::address_from_storage` 决定），`-O2/-O3` 走 SSA 往返（对象身份由
/// `mem/object_resolver.rs::resolve_identity` 的查表顺序 + `opt.rs` 的
/// 「带运行时尺寸对象不进 IR」闸决定）。修前实测：开工基线四档全
/// SIGSEGV(139)；只修消费侧基址时 -O2/-O3 的 `sizeof` 读回 0（同形状 252 vs 255）。
#[test]
fn vla_runtime_size_identity_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/vla_runtime_size_identity.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("vla-runtime-identity", src, &[flag]),
            0,
            "{flag}: VLA/VM 对象的基址或运行时尺寸在档位之间不再是同一个对象"
        );
    }
}

// ============================================================================
// Phase 5.1 闭环后：延伸池（caller-saved r8/r9）默认开启，发号与暂存约定已对齐。
//
// 本用例钉的性质从「延伸池不许发家」翻转成两条可观测不变式：
//   1. 池满时确实走 spill 路径（stats.poolfull > 0），而不是静默拒绑或拿有主号；
//   2. 真正的通用 scratch 号（%rcx/%rsi/%rdi——SysV 实参装载与 varargs 计数
//      的约定号）永远不会被当成家发出去（它们在 auto_pool 候选表外）；
//   3. 运行值与 gcc 对齐（开池后若有静默错值，此处立刻红）。
//
// 旧用例断言「%r8/%r9 不许当_home」是 Phase 5.1 前置三未做完时的结论；当
// `persist_ext_homes`/`restore_ext_homes` 与 `pick_rhs_stage` 就位后，这两个号
// 转正为默认候选（见 `src/ssa/regalloc.rs::AUTO_EXT_POOL` 与 §11.54 闭环说明）。
// ============================================================================

#[test]
fn ext_pool_hands_out_no_homes_until_scratch_respects_occupancy() {
    // 12 个依次短命、各跨一次调用的局部：callee-saved 池满后延伸池必须顶上，再满就 spill。
    let src = r#"
static long acc;
long eat(long k) { acc += k; return k * 3 + 1; }
long f(long k) {
    long a = eat(k + 1); long b = eat(k + 2); long c = eat(k + 3);
    long d = eat(k + 4); long e = eat(k + 5); long g = eat(k + 6);
    long h = eat(k + 7); long i = eat(k + 8); long j = eat(k + 9);
    long m = eat(k + 10); long q = eat(k + 11);
    acc += a + b + c + d + e + g + h + i + j + m + q;
    return acc;
}
int main(void) { long r = f(7); return (int)(r % 251); }
"#;
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-extoff-{}.c", std::process::id()));
    fs::write(&sp, src).expect("写夹具");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-O2")
        .arg("-S")
        .arg("-o")
        .arg("/dev/null")
        .arg(&sp)
        .env("DCC_REGALLOC_DUMP", "1")
        .output()
        .expect("跑 dcc");
    let diag = String::from_utf8_lossy(&out.stderr).to_string();
    // 池必须真的满：callee-saved + 延伸池不够分，后面那批必须走 spill。
    // （否则下面「延伸池当 home」是在测一条不会触发的路径，空跑。）
    assert!(
        diag.split_whitespace()
            .any(|tok| tok.strip_prefix("poolfull=").and_then(|v| v.parse::<usize>().ok()).map_or(false, |n| n > 0)),
        "被测源必须把自动池占满并出现 spill（否则本用例是空跑）：\n{diag}"
    );
    // 延伸池号 r8 应当被当 home 发出去（Phase 5.1 转正后的可见证据）。
    assert!(
        diag.contains("-> %r8 "),
        "延伸池 %r8 没被发成家 —— 要么池没满、要么候选表漏了 r8：\n{diag}"
    );
    // 真正的通用 scratch 号（SysV 实参装载与 varargs 计数约定）永远不许当 home。
    for r in ["%rcx", "%rsi", "%rdi"] {
        assert!(
            !diag.contains(&format!("-> {r} ")),
            "scratch 号 {r} 被当成家发出去了 —— 这会踩到实参装载/varargs 约定，产出静默错值。"
        );
    }
    let _ = fs::remove_file(&sp);
}

/// 嵌套聚合成员赋值（`o.a.b = v` / `p->u.s = v` / `(*pq).o.in = v`）逐档与 gcc 对照。
///
/// 守的是同一条机器上的三个已观测失效（sqlite `yy_shift` 是第一例，实测把整条
/// 解析器栈写坏后崩溃）：
///   1. 字段是聚合时 `MemberStore` 的基址按**右值**降级 —— struct/union 基址于是变成
///      整块拷贝进帧临时，写落在副本上（`yytos->minor.yy0 = yyMinor` 丢写）；
///   2. 箭头分支前置的 `Load{e: p, ty: fld_ty}` 把字段按值读进临时再 dot 写，同样丢写；
///   3. 解引用基址 `(*pq).o` 经 `lower_expr` 是聚合拷贝，被当成「地址」用之后，
///      codegen 从对象头 8 字节取指针再写 ⇒ 段错误（夹具 `three_levels`）。
/// 三处现在共用 `member_base_is_ptr_operand` + `lower_lvalue_base_addr` +
/// `lower_member_addr` 这一套地址归一化（政策见各函数注释）。
#[test]
fn agg_member_store_base_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "agg-member-store-base",
        include_str!("../fixtures/opt/agg_member_store_base.c"),
        "聚合成员写的基址被按右值物化（写进帧临时）/ 解引用基址被当指针用（段错误）",
    );
}

/// 指针字段的下标基址（`&p->zText[k]` / `p->zText[k] = c`）逐档与 gcc 对照。
///
/// `lower_lvalue_base_addr` 的 `Member` 分支一律委托 `lower_member_addr` 时，
/// 把「字段地址」当成了「下标基址」：`&p->zText[k]` 变成结构体地址+偏移+k，
/// `sqlite3_str_append` 的 `memcpy` 于是把整条字符串写进 `StrAccum` 自己身上，
/// `zText` 被覆盖后 `sqlite3StrAccumFinish` 解引用垃圾 ⇒ `sqlite3_mprintf` 全崩。
/// 字段是聚合/数组时字段地址才是对的（`agg-member-store-base` 那条），
/// 所以这个分支必须按**字段类型**分流。
#[test]
fn member_ptr_subscript_base_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "member-ptr-subscript-base",
        include_str!("../fixtures/opt/member_ptr_subscript_base.c"),
        "指针字段的下标基址被按字段地址物化（memcpy 写进结构体自身）",
    );
}

/// `&&` / `||` 的右操作数被落到守卫之外（SSA 通道）逐档与 gcc 对照。
///
/// SSA 通道不为短路求值造块：整个条件是一棵交给重建的表达式树，树里每个值都必须内联
/// 进树。某个值被「落到」块里的独立赋值（`is_addr_producer` 的 `MemberAddr`/`IndexAddr`
/// 永不内联，内联窗口判据又因它自己的定义点而放弃）时，它按块序发射在带守卫的分支
/// **之前** ⇒ 被守卫保护的那次求值提前发生：`count(*)` 的
/// `pList && pList->nExpr > db->aLimit[..]`（`pList` 恒为 0）直接段错误。
/// 处置是 correct-by-bail：整函数退出 SSA 往返，保留 AST 原形。
#[test]
fn lazy_guard_and_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "lazy-guard-and",
        include_str!("../fixtures/opt/lazy_guard_and.c"),
        "短路右操作数被落到守卫之前求值（空指针段错误 / 走进错误分支）",
    );
}

/// 汇聚块被一条臂吃掉后，兄弟臂丢掉尾部 `return`（`%eax` 留着旧值）逐档与 gcc 对照。
///
/// 重建的直线守卫用 `cx.visited` 保证「一个块只展开一次」。它的正确性前提是：块一旦
/// 被某条臂展开，其支配者集合里的其它臂走到这里时可以直接跳过。`else if` 阶梯破坏这个
/// 前提——`branch_join` 只沿 `Jump` 链找汇聚点，而提前 `Ret` 的臂只剩虚拟 EXIT 这个
/// 后支配者 ⇒ 阶梯的 join 为 `None`，第一条臂把尾部共享块就地内联并标记 visited，
/// 兄弟臂再走到它就直接返回，那条 `return <val>` 从函数里消失，`%rax` 留着上一次求值
/// 的旧值。实测 `decodeFlags()`（sqlite3.c）对内部页返回 11 而非 5 ⇒
/// `btreeInitPage` 失败 ⇒ 凡表 B-tree 有内部根页的库都报 "database disk image is
/// malformed"。处置：`Ret` 终结块（无后继 ⇒ 重展开有界）允许再次展开。
#[test]
fn join_return_ladder_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "join-return-ladder",
        include_str!("../fixtures/opt/join_return_ladder.c"),
        "汇聚块被一条臂展开后，兄弟臂跳过它并丢掉尾部 return（%eax 留着旧值）",
    );
}

/// 两条臂**各自又是一层分支**时，If 之后的公共尾部被第一条臂吃掉、兄弟臂整段丢失，
/// 连 `return` 的取值都不再物化（`%rax` 留着上一条指令的旧值）—— 逐档与 gcc 对照。
///
/// 与上一条同族，但落空的是 `branch_join` 的全部三条判据：臂不是 `Jump` 直链
/// （`walk_to_join` 在臂的第一条 `Branch` 就返回 `None`）、臂内有早退 `Ret`
/// （`from` 没有真实后支配者，ipdom 只剩虚拟 EXIT 占位名，被上层过滤成 `None`）、
/// 非对称可达那步也没有候选块可用 ⇒ `join=None`，两条臂自由展开，公共尾部只存在于
/// 第一个到达它的那条臂里。实测 `sqlite3SrcListAppend()`（sqlite3.c）正是这个形状：
/// else 臂丢掉 `pItem=&pList->a[…]` 与 `return pList`，把 `nSrc` 当指针返回，
/// `selectExpander` 随后解引用空 SrcList ⇒ 任何两表 JOIN 段错误。
/// 处置：`arm_join` / `side_only_exits` 允许穿过臂自己的嵌套分支找汇聚块。
#[test]
fn nested_arm_join_tail_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "nested-arm-join-tail",
        include_str!("../fixtures/opt/nested_arm_join_tail.c"),
        "两臂各自又是分支时找不到汇聚块，兄弟臂丢掉 If 之后的公共尾部与 return 取值",
    );
}

/// `||` / `&&` 右操作数带副作用或可陷获时，SSA builder 必须整函数退回 AST 通道。
///
/// SSA 通道不为短路求值造块，右操作数会被急切求值。`Expr::Binop` lowering 因此对
/// `&&`/`||` 检查右操作数降级出的指令纯度（`emitted_is_pure`，与三元 `Cond` 同判据）：
/// store / load / 调用 / 除法 ⇒ `unsupported`。实测 sqlite `walIndexPage` 的
/// `nWiData<=iPage || (*ppPage = apWiData[iPage])==0`：`apWiData==NULL` 时右操作数
/// 照样求值，`mov (%rax),%rax` 打空 ⇒ WAL 模式必崩（-O0/-O1 与 gcc 正常）。
#[test]
fn short_circuit_rhs_impure_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "short-circuit-rhs-impure",
        include_str!("../fixtures/opt/short_circuit_rhs_impure.c"),
        "|| 右操作数带 store/load 被急切求值：左真时打空或写不该写的槽",
    );
}

/// `&(ptr_array[0][k])` 的下标基址必须取指针**值**而不是槽地址。
///
/// `lower_lvalue_base_addr` 旧实现对任意内层 `Index` 都发 `IndexAddr`；那只对结果类型
/// 是对象（数组行/结构体/联合体）成立。实测 sqlite `walCkptInfo` 的
/// `&(pWal->apWiData[0][sizeof(WalIndexHdr)/2])` 少一次取指针值的加载，
/// 返回 `apWiData+96` 而非 `apWiData[0]+96`，`WalCkptInfo` 的写踩坏堆 ⇒
/// `malloc(): invalid next size` 中止。结构体元素（`&db->aDb[0].pBt`）仍走 IndexAddr，
/// 否则元素被按值拷进帧临时、写落进副本（sqlite3BtreeOpen 段错误）—— 两侧都由本夹具钉住。
#[test]
fn index_ptr_base_double_index_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "index-ptr-base-double-index",
        include_str!("../fixtures/opt/index_ptr_base_double_index.c"),
        "指针数组元素的双重下标取址被算成槽地址+偏移：写落进指针数组旁边的堆内存",
    );
}

/// 函数局部 `static` 指针变量的字符串初始化器必须发射 .rodata 重定位。
///
/// `register_static_global` 旧实现只 `resize` 出 sz 个零字节、不发重定位 ⇒ 局部 static
/// 指针恒为 NULL（各级别均复现；文件作用域全局走的是另一条正确路径）。实测 sqlite
/// `shell.c` 的 `zAutoColumn`：`zTabFill` 为 NULL，`.import` 自动列名路径上
/// `sqlite3_prepare_v2(db, NULL, ...)` 返回 SQLITE_MISUSE，`rc_err_oom_die` 断言中止。
#[test]
fn static_local_ptr_init_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "static-local-ptr-init",
        include_str!("../fixtures/opt/static_local_ptr_init.c"),
        "函数局部 static 指针的初始化器只填零不发重定位：指针恒为 NULL",
    );
}

/// 投机执行（P14 specex）的上提切片不得跨过「读写同一基名」的指令，判据按**基名**比。
///
/// `slice_is_movable` 旧实现拿 SSA 版本名比（只在 version==0 时等于基名）且只查一个
/// 方向。重建按基名塌缩成可变量后，被跨过的读会看见另一个版本的值。实测 sqlite
/// `sqlite3GetVarint`：`b = p[1]`（读 `p.0`）被切片中 `p += 2`（写 `p.1`）的上提跨过 ⇒
/// 4 字节 varint 解码错，症状是 18000+ 行 bulk insert 到 INTEGER PRIMARY KEY 表报
/// "UNIQUE constraint failed"（rowid ≥ 16384 走 3/4 字节 varint 路径）。
#[test]
fn specex_base_name_cross_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "specex-base-name-cross",
        include_str!("../fixtures/opt/specex_base_name_cross.c"),
        "投机上提跨过对同一基名的读写：塌缩后读到另一版本的值（varint 解码错）",
    );
}

/// -O3 循环分段不得重排体内可观察副作用的顺序，也不得让未被屏蔽的 `break` 多跑迭代。
///
/// 见文件头第 11 条。两条顺序敏感探针都在夹具里：逐条 printf（输出字节序就是 (i,k) 序列）
/// 与十进制进位累加（把整段顺序压进一个整数，打进 stdout）。
#[test]
fn tile_strip_order_break_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "tile-strip-order-break",
        include_str!("../fixtures/opt/tile_strip_order_break.c"),
        "-O3 分块按块重排迭代顺序 / 未被屏蔽的 break 在分段后多跑迭代",
    );
}

/// 混合符号整型比较的公共类型（C11 6.3.1.8 的可表示性判据）、常量折叠器的符号性判据、
/// SSA 常量类型还原（宽字面量与传播后的小常量）。
///
/// 见文件头第 12—15 条，四个站点：① `usual_arith_type` 缺可表示性判据（**-O0 就错**，
/// 拿 -O0 当基准的跨档对拍看不见它）；② SSA 常量无类型、重建按量级阶梯猜（只在 -O2/-O3
/// 错）；③ `cmp_binop_values` 自拍判据 + `as u128 as i128` 往返恒等（四档全错）；
/// ④ 小常量歧义 ⇒ 必须按指令 `dest` 名记操作数类型（-O2/-O3 错）。四个方向都由本夹具
/// 钉住，所以它必须同时与 gcc 对拍且逐档跑。
#[test]
fn mixed_signedness_usual_arith_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "mixed-signedness-usual-arith",
        include_str!("../fixtures/opt/mixed_signedness_usual_arith.c"),
        "usual_arith_type 缺可表示性判据 / SSA 常量重建丢字面量无符号性 / 折叠器符号性判据 / 传播后小常量歧义",
    );
}

/// P13 阶段3（条件直生分支）的产物级对拍。快照用例只能钉「发了哪条 `jcc`」的形状，
/// 钉不了极性：条件码取反写错（`inverse()` 漂移）或令牌泄到「值还要用」的嵌套比较上
/// 时，形状仍然是合法的，只有运行结果会错。本夹具逐条 `chk`，退出码 = 判错条数，
/// 四档都得与 gcc 一致。
#[test]
fn direct_cond_branch_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "direct-cond-branch",
        include_str!("../fixtures/opt/direct_cond_branch.c"),
        "条件直生分支的条件码/极性/消费式令牌不得泄给嵌套比较/白名单外仍材料化",
    );
}

/// 浮点条件必须测**浮点值本身**（`xmm0`），而不是上上次求值留在 `rax` 的残留。
/// HEAD 与 gcc 一致纯属巧合（整数比较恰好把 0/1 留在了 rax）；条件直生分支拿掉
/// `setcc+movzx` 后那个残留不再是 0，`gcc.dg/tree-ssa/scev-2.c` 当场 abort。本夹具
/// 把五种条件语境（if/while/for/do-while/三元）× 零/非零/负值/运行时 NaN 全部钉住，
/// 并且要求逐档与 gcc 一致（NaN 必须判真 ⇒ 必须复用带 PF 项的浮点比较）。
#[test]
fn float_truth_condition_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "float-truth-condition",
        include_str!("../fixtures/opt/float_truth_condition.c"),
        "emit_truth_flags 的 SSE 浮点臂缺失（浮点条件读陈旧 rax）",
    );
}

/// 标量写直生（P13 阶段2）的行为对拍：白名单那条臂把地址直接算进 `rcx`、
/// 右值算进 `rax`，取消了「押栈到帧里再 pop 回来」；名单外（Binop/Unary/Call/下标取址
/// 右值、复合赋值、浮点与 16 字节值通道、聚合拷贝、位域 RMW）必须照原样。
#[test]
fn scalar_store_direct_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "scalar-store-direct",
        include_str!("../fixtures/opt/scalar_store_direct.c"),
        "赋值白名单误收/误排就会在某一档上错值或错宽度",
    );
}

/// P13 阶段2 尾项：地址临时值的**唯一**豁免窗口（`inline_temps::addr_temp_folds_at_sole_memop`
/// + `expr_rebuild::fold_direct_named_access`）。折叠把 `*(&sym) = v` 还原成对 `sym` 的直接写，
/// 于是那条 `lea` 与它的落槽写一起消失；夹具的每一段都在钉一条判据的**反面**：
/// volatile、指针算术、再套一层取址、地址进 call、同一地址用两次、定义与使用不紧邻、
/// 取址逃逸后「直接写/经指针写」交替、循环里每轮重新取址。
#[test]
fn addr_temp_fold_direct_access_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "addr-temp-fold-direct-access",
        include_str!("../fixtures/opt/addr_temp_fold_direct_access.c"),
        "豁免窗口放宽成「通用地址内联」就会在指针算术/volatile/逃逸段静默错值",
    );
}

/// P13 阶段5：-O2 的成本模型必须能把**带控制流**的单点 `static` 助手展开到调用点。
///
/// 判据取产物级面：展开后不应再存在向该函数的 `call`（展开前必然有一个）。
/// 为什么盯「单点」：多调用点的函数复制多份会把 I-cache 代价换成 call 收益（实测
/// Lua fib 统一阈值时反而慢 2.5%），单点展开不复制代码，是纯收益的那一半。
#[test]
fn o2_static_inline_cost_model_inlines_small_helper() {
    use std::fs;
    use std::process::Command;
    let src = "\nstruct S { int a; int b; };\n\
               static int helper(struct S *p, int x) {\n\
                   int n = 0;\n\
                   if (x > 0) { p->a = x; n = 1; } else { p->b = -x; n = 2; }\n\
                   while (n > 0) { n--; }\n\
                   return p->a + p->b;\n\
               }\n\
               int entry(int x) { struct S s; s.a = 0; s.b = 0; return helper(&s, x); }\n";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src_path = dir.join(format!("dcc-static-inline-{pid}.c"));
    let asm_path = dir.join(format!("dcc-static-inline-{pid}.s"));
    fs::write(&src_path, src).expect("write");
    for level in ["-O2", "-O3"] {
        let _ = fs::remove_file(&asm_path);
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args([level, "-S", "-o"])
            .arg(&asm_path)
            .arg(&src_path)
            .output()
            .expect("run dcc");
        assert!(out.status.success(), "{level} stderr: {}", String::from_utf8_lossy(&out.stderr));
        let text = fs::read_to_string(&asm_path).expect("read asm");
        let calls = text
            .lines()
            .filter(|l| l.trim_start().starts_with("call"))
            .filter(|l| l.contains("helper"))
            .count();
        assert_eq!(
            calls, 0,
            "{level}: 单点 static 助手应当被展开，但仍留下了对 helper 的 call：\n{text}"
        );
    }
    let _ = fs::remove_file(src_path);
    let _ = fs::remove_file(asm_path);
}

// ============================================================================
// P13 发射形态判据（§7 清单的落地，逐条对照见方案 §13.9）
//
// 这一组问的不是「算对没有」（那是上面那些 `_matches_gcc_at_all_levels` 的事），而是
// 「发射层最后发的到底是哪条指令」—— 形状退化了运行结果依然是草的对的，所以只能看产物。
// 判据一律取两头：要么定一个必须出现的形态，要么定一个必须不出现的形态；
// 只定「出现」的话，「多发了一条死指令」这整类回归都看不见。
// ============================================================================

/// P13 阶段1/2（§3.1/§3.2）：标量常量写不得再走「押栈躲 rax 复用」那条路。
///
/// 旧形态是 7 条：`lea` 取址 + 落槽写 + `push %rax` + 右值 + `movsxd` + `pop %rcx` +
/// `mov %eax,(%rcx)`（gcc 是 1 条 `movl $1,g(%rip)`）。修完应当是「右值进 rax，
/// 直接 RIP 相对直写」。判据取两头：① 存储确实是 RIP 相对直写；② 除帧指针外零 push/pop。
#[test]
fn o2_scalar_store_of_constant_emits_no_frame_round_trip() {
    let body = harness::dcc_fn_body_at(
        "p13-store-const",
        "int g;\nvoid store_const(void) { g = 1; }\n",
        "-O2",
        "store_const",
    );
    assert!(
        body.contains("g(%rip)"),
        "-O2: 常量写没有落到 RIP 相对直写上（§3.1 的地址临时值形态又回来了）：\n{body}"
    );
    // 非空性：帧指针本身仍然发 `push %r13`/`pop %r13`，所以下面那个 0 是「除了帧指针
    // 没有别的 push/pop」，不是「解析器一条 push 都没看到」。
    assert!(
        harness::count_mnemonic(&body, "push") >= 1,
        "-O2: 函数体里连帧指针的 push 都没看到，判据失效：\n{body}"
    );
    assert_eq!(
        harness::non_frame_push_pop(&body),
        0,
        "-O2: 常量写又回到「押栈躲 rax 复用」的老形态：\n{body}"
    );
}

/// P13 阶段2：右值已经落在寄存器里、且不会被后续求值覆盖时，赋值臂不得再 push/pop。
///
/// 与上一条的区别是右值是**变量**而不是常量：旧的 push/pop 是为了把「地址」藏到帧里
/// 躲 ax 复用；`store_rhs_isolated` 的判据是「右值只读一个已在寄存器/帧槽的值」
/// ⇒ 地址直接进 rcx，不必押栈。
#[test]
fn o2_assign_with_isolated_rhs_skips_push_pop() {
    let body = harness::dcc_fn_body_at(
        "p13-store-val",
        "int g;\nvoid store_val(int v) { g = v; }\n",
        "-O2",
        "store_val",
    );
    assert!(body.contains("g(%rip)"), "-O2: 存储不是 RIP 相对直写：\n{body}");
    assert_eq!(
        harness::non_frame_push_pop(&body),
        0,
        "-O2: 隔离右值的赋值仍在押栈：\n{body}"
    );
}

/// P13 阶段3：整型比较作为 `if` 的条件时必须直生 `cmp; jcc`，不得先材料化 `setcc`。
///
/// 旧形态 5 条（`cmp; setcc; movzx; test; jcc`，依赖深度 5），gcc 是 2 条。
/// 判据取两头：有且至少一条条件跳转，且整函数零 `setcc`。
#[test]
fn o2_int_compare_in_if_emits_jcc_without_setcc() {
    let body = harness::dcc_fn_body_at(
        "p13-cmp-int",
        "int f(int a, int b) { if (a < b) return 1; return 0; }\n",
        "-O2",
        "f",
    );
    assert!(
        harness::count_cond_jump(&body) >= 1,
        "-O2: 整型条件没有直生条件跳转：\n{body}"
    );
    assert_eq!(
        harness::count_mnemonic_prefix(&body, "set"),
        0,
        "-O2: 条件被消费的语境仍发了 setcc（回到 5 条形态）：\n{body}"
    );
}

/// P13 阶段3：无符号比较必须选无符号条件码（`jae`/`jb`），而不是先 `setae` 再测。
///
/// 极性写错时形状依旧合法，只有运行结果会错（`mixed_signedness_usual_arith` 与
/// `direct_cond_branch` 两个夹具从执行侧钉这件事）；本条只钉「发射层选的是哪一侧」。
#[test]
fn o2_unsigned_compare_in_if_uses_jae_not_setae() {
    let body = harness::dcc_fn_body_at(
        "p13-cmp-uns",
        "int f(unsigned a, unsigned b) { if (a < b) return 1; return 0; }\n",
        "-O2",
        "f",
    );
    assert!(
        body.lines()
            .any(|l| l.trim_start().starts_with("jae ") || l.trim_start().starts_with("jnb ")),
        "-O2: 无符号 `<` 的直生分支没有用 jae/jnb（无符号侧条件码选错了）：\n{body}"
    );
    assert!(!body.contains("setae"), "-O2: 仍先 setae 再测：\n{body}");
}

/// 窄类型（`signed char`）比较：条件被消费时直生，值要用时仍必须材料化。
///
/// 计划里这条原名 `o2_narrow_char_compare_still_materializes_bool`，假设窄类型比较会
/// 落在直生白名单之外；实测（本用例首跑）它属于白名单内（操作数先 `movsbq` 提升到
/// 32 位再比），所以断言改成正反两面：
/// ① `if (a == b)` —— 零 `setcc`、有 `jcc`；② `return a == b;` —— 必须有 `setcc`。
/// 「材料化」这个要求本身没变，只是它只对「值要用」那一侧成立。
#[test]
fn o2_narrow_char_compare_branches_directly_but_materializes_when_value_used() {
    let src = "int narrow_if(signed char a, signed char b) { if (a == b) return 1; return 0; }\n\
               int narrow_bool(signed char a, signed char b) { return a == b; }\n";
    let branch = harness::dcc_fn_body_at("p13-narrow-if", src, "-O2", "narrow_if");
    assert_eq!(
        harness::count_mnemonic_prefix(&branch, "set"),
        0,
        "-O2: 窄类型比较作为条件时仍发了 setcc：\n{branch}"
    );
    assert!(harness::count_cond_jump(&branch) >= 1, "-O2: 窄类型条件没有直生分支：\n{branch}");

    let value = harness::dcc_fn_body_at("p13-narrow-bool", src, "-O2", "narrow_bool");
    assert!(
        harness::count_mnemonic_prefix(&value, "set") >= 1,
        "-O2: 比较结果被当作值返回时没有材料化成 0/1：\n{value}"
    );
}

/// P13 阶段3（三元）：`c ? a : b` 的条件也不得出落点材料化，只改「条件码怎么被消费」。
#[test]
fn o2_ternary_condition_emits_jcc_for_int_compare() {
    let body = harness::dcc_fn_body_at(
        "p13-ternary",
        "int f(int a, int b) { return a < b ? 10 : 20; }\n",
        "-O2",
        "f",
    );
    assert!(
        harness::count_cond_jump(&body) >= 1,
        "-O2: 三元条件没有直生条件跳转：\n{body}"
    );
    assert_eq!(
        harness::count_mnemonic_prefix(&body, "set"),
        0,
        "-O2: 三元条件仍先材料化：\n{body}"
    );
}

/// 浮点比较的白名单反面：必须**继续材料化**，且必须保住带 PF 项的双条件。
///
/// 浮点条件不能直生的理由与整数不同：`a < b` 在 NaN 下为假，单看 CF（`setb`）会把
/// 「无序」误判成真，所以形态只能是 `ucomisd; setb; setnp; and`（或等价的双跳转）。
/// 这里钉的是 HEAD 的形态仍然在，避免「为了减一条 setcc 而把 PF 页丢了」这类回归
/// —— `gcc.dg/tree-ssa/scev-2.c` 当年就是被这条读陈旧 rax 打红的。
#[test]
fn float_compare_in_if_still_materializes_bool() {
    let body = harness::dcc_fn_body_at(
        "p13-cmp-double",
        "int f(double a, double b) { if (a < b) return 1; return 0; }\n",
        "-O2",
        "f",
    );
    assert!(
        body.contains("ucomisd") || body.contains("comisd"),
        "-O2: 浮点比较没有走 SSE 比较：\n{body}"
    );
    assert!(
        harness::count_mnemonic(&body, "setb") >= 1,
        "-O2: 浮点 `<` 不再材料化 CF：\n{body}"
    );
    assert!(
        harness::count_mnemonic(&body, "setnp") >= 1,
        "-O2: 浮点比较丢了 PF（setnp）—— NaN 会被判成 `a < b` 为真：\n{body}"
    );
    assert!(
        body.contains("testl"),
        "-O2: 材料化后的 0/1 不再被 test 消费：\n{body}"
    );
}

/// 位域比较的白名单反面：位域没有自己的地址，先把容器取出再 `and` 掩码，所以它的
/// 「条件」在发射层看起来与普通整型比较同形 —— 但它必须**继续**走材料化。
///
/// 为什么不能直生：位域读取有额外的掩码/符号扩展步骤与「容器宽度」语义，直生分支
/// 要复用的中间值比整数比较多一个；§9.2 那条位域 RMW 错值就出在同一层。
#[test]
fn bitfield_compare_in_if_still_materializes_bool() {
    let body = harness::dcc_fn_body_at(
        "p13-cmp-bitfield",
        "struct B { unsigned f : 3; unsigned pad : 29; };\n\
         int f(struct B *p) { if (p->f == 5) return 1; return 0; }\n",
        "-O2",
        "f",
    );
    assert!(
        harness::count_mnemonic_prefix(&body, "set") >= 1,
        "-O2: 位域比较不再材料化（形状变了就要重跑 §9.2 的夹具）：\n{body}"
    );
    assert!(
        body.contains("testl"),
        "-O2: 位域比较的材料化值没被 test 消费：\n{body}"
    );
}

/// §2.2 的负结果防线：「落点是下一条指令」的 `jmp` 目前**必须保留**。
///
/// 那次按原判据删掉 135 条恒定跳转（静态 −675 字节），同机交替 best-of-9 实测
/// min 310.9→322.6ms、p50 319.3→326.4ms（慢 3.8%）：5 字节×135 的位置移动把热循环体
/// 推离了原有对齐，布局代价盖过省下一条跳转。计划里那条 `jmp_to_next_is_elided_at_emit_time`
/// 因此按「前提已不存在」换成这条反向防线 —— 要翻它，必须先把对齐作为控制变量的 A/B 补上。
#[test]
fn jmp_to_next_is_still_emitted_after_layout_negative_result() {
    let body = harness::dcc_fn_body_at(
        "p13-jmp-to-next",
        "int f(int x) { int r = 0; if (x) r = 1; return r; }\n",
        "-O2",
        "f",
    );
    assert!(
        harness::count_jmp_to_next(&body) >= 1,
        "-O2: 恒定跳转被删了 —— §2.2 已实测同形改动慢 3.8%，翻这条必须先补对齐 A/B：\n{body}"
    );
}

/// 链式赋值的**每一个**全局标量写都必须落地（发射层窥孔的占位寻址判据）。
///
/// `d = b = 1;` 脱糖成两条相邻的 `mov %eax,<符号>(%rip)`。RIP 相对的 disp32 在指令字节里
/// 就是 `00 00 00 00` 占位，真实偏移靠链接器按符号补丁填 ⇒ 这两条**字节完全相同**，
/// 「相邻两条同字节幂等搬运 ⇒ 第二条不发」把外层那次写整条削掉。
///
/// 为什么只靠产物看不住：符号补丁是调用点在 `emit_insn` **返回之后**按 disp32 位置
/// 自行登记的（`mem/access.rs` 里 `lea r11, sym(%rip)` 紧接 `pending_data_rels.push` 那一
/// 型），而 `emit_insn` 的 `true` 说的是「编码器覆盖了该形态」，不是「字节已落地」。
/// 于是补丁记录留在台账上，链接器把那 4 字节写进**下一条指令的 ModRM 字段**
/// （实测 `movslq 0x0(%rip),%rax` 被改成 `movslq 0x2095(%rdx),%rsi`，基寄存器从未初始化）。
/// 症状是间歇的：.text 字节完全确定（连编三次哈希一致），跑不跑得起取决于进程启动时
/// `%rdx` 的残值（同一 exe 跑 30 次：12 通过 / 18 段错误）—— 所以只有逐档执行对照能
/// 稳定接住它，反汇编快照那类「只看文本」的用例反而会把它当正常形态录进去。
#[test]
fn chain_assign_global_store_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "chain-assign-global-store",
        include_str!("../fixtures/opt/chain_assign_global_store.c"),
        "窥孔把带 %rip 占位的第二条同字节搬运当冗余削掉，链接期补丁写进下一条指令的 ModRM 字节",
    );
}

/// 分支语境的 `A || B` 必须保持短路顺序（`jump_if_true == false` 那条分解臂）。
///
/// 原注释断言「`!(l && r)` 与 `!(l || r)` 都等价于任一侧为假就跳」，这是错的：
/// `!(l || r)` 是「两侧都假才跳」，左真时**不能**跳，必须落到右操作数。照 `&&` 的拆法
/// 编 `if (a || f())` 会把短路整条反向（左假提前跳出、左真反而求值右值）。
///
/// `short-circuit-rhs-impure` 从 SSA 闸门侧接住了它（那里的右值带 load，急切求值会打空），
/// 本夹具从**分支语境**直接接住：左真时右值的 store 不许发生、左假时右值必须发生，
/// 并顺带钉住 `a || (b && c)` 的递归分解与 `while (a || b)` 的反向臂。
/// main 里的判断一律只用单侧比较与 `&&` —— 被测的正是 `||`，再拿 `||` 判结果会连
/// 「报错」这一步一起错掉（自举盲区）。
#[test]
fn lor_branch_order_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "lor-branch-order",
        include_str!("../fixtures/opt/lor_branch_order.c"),
        "`||` 在假则跳出语境被按 `&&` 拆，短路整条反向",
    );
}

/// `__int128` 在真值语境必须按两半判（`emit_truth_flags` 的兜底形态）。
///
/// 本后端 16 字节整型的 rvalue 是 `(rax, rdx)` 一对，而那条兜底过去只按 `ty.size() > 4`
/// 选 `test %rax,%rax` ⇒ 「低 64 位为 0、高 64 位非 0」被判成假。漏点只在条件被分解成
/// 「测值」的三条入口上（`x != 0` / `!x` / `x || y`），比较运算本身走 `gen_int128_binop`
/// 两半都比，所以错值不会在大多数语料里现形。
///
/// 为什么必须单独建夹具：反汇编快照 `pr105613` 早就现过形（`r[0] != 0` 被编成一条
/// `test %rax,%rax`），可那个语料只取 lo!=0 与全 0 两种数据，**恰好不含 hi-only**，
/// 于是它长期只在快照文本里可见、执行侧永远绿。快照与语料数据的组合盲区不是运气，
/// 要靠专门取 hi-only 数据的执行级用例补上。
#[test]
fn int128_truth_condition_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "int128-truth-condition",
        include_str!("../fixtures/opt/int128_truth_condition.c"),
        "128 位真值判据只测低半：hi!=0 且 lo==0 被误判为 0",
    );
}

/// 阶段2 白名单的第一条正面：右值是**整型常量表达式**时，标量写不得押栈。
///
/// 这条判据与 `dispatch.rs` 里 `Expr::Binop` 臂的常量折叠**逐字同源**（见
/// `binop_const_foldable`）：折成功就只剩一条 `mov imm`，只写 `rax`，地址一条
/// `mov rcx, rax` 就到位。lvm.c 里 `settt_()` 展开的 `(3|(0<<4))` 这一类就是这个形状，
/// 它们本来在发射层就会折，只是 AST 层的形态判据把它们挡在名单外、白白押栈。
///
/// 左值特意取 `*q`（q 是**全局指针**）：这条地址链描述不成 `LateAddr`（重算就要重新读
/// 全局里那个指针值，而右值可能改写它），所以「零押栈」只能由 `InRcx` 给出 —— 摘掉
/// `binop_const_foldable` 就会退到下面那条反向对照的形态，本用例立即变红。
#[test]
fn o2_const_expr_rhs_store_skips_push_pop() {
    let body = harness::dcc_fn_body_at(
        "p13-const-expr-rhs",
        "int *q;\nvoid store(void) { *q = (3 | (0 << 4)); }\n",
        "-O2",
        "store",
    );
    assert!(
        body.contains("%rip"),
        "-O2: 全局指针取址没有走 RIP 相对（判据源换了形）：\n{body}"
    );
    // 非空性：帧指针本身仍发 `push %r13`，所以下面那个 0 是「除了帧指针没有别的
    // 押栈」，不是「解析器一条 push 都没看到」（同 §7 那条常量写用例的同一道锚）。
    assert!(
        harness::count_mnemonic(&body, "push") >= 1,
        "-O2: 函数体里连帧指针的 push 都没看到，判据失效：\n{body}"
    );
    assert_eq!(
        harness::non_frame_push_pop(&body),
        0,
        "-O2: 常量表达式右值的标量写又回到「押地址躲 rax 复用」：\n{body}"
    );
}

/// 阶段2 追加（Late）：地址链可重算、右值只踩 rcx/rdx 而不会改地址时，标量写不得押栈。
///
/// 依的是 C11 6.5.16「赋值两个操作数求值无序」：先算右值，随后一条 `lea`/`mov` 重算
/// 同一个地址。这里 `p->n = p->n + 1` 的右值**不会**进 `InRcx` 名单（`scalar_load_shape`
/// 对 `Binop` 只收 `binop_const_foldable`，而含 `p->n` 的加法折不动），也不该押栈 ——
/// luaV_execute 的主力形态 `L->top = L->top + 1` 正好在这一条上。`Index` 与带 VLA 的
/// 成员偏移仍在名单外（下标要算进 rcx，重算等于重复求值下标）。
#[test]
fn o2_late_addr_recompute_skips_push_pop() {
    let body = harness::dcc_fn_body_at(
        "p13-late-addr",
        "struct S { int n; int m; };\nvoid bump(struct S *p) { p->n = p->n + 1; }\n",
        "-O2",
        "bump",
    );
    // 非空性：同上，没有这道锚则「zero」本身可能就是解析没看到的产物。
    assert!(
        harness::count_mnemonic(&body, "push") >= 1,
        "-O2: 函数体里连帧指针的 push 都没看到，判据失效：\n{body}"
    );
    assert_eq!(
        harness::non_frame_push_pop(&body),
        0,
        "-O2: 可重算地址的标量写仍在押栈（Late 那条路没接上）：\n{body}"
    );
}

/// 两条白名单的反面：地址链不可重算 **且** 右值可能改写它时，必须**保留**押栈。
///
/// 判据类用例只钉一面：只写「命中时不押栈」的话，有人把保守门拆掉（比如允许 `Index`
/// 重算、或对 call 右值也放行 Late）会静默做错值，而产物上反而更「优」。本条把
/// `*q = f()` 钉在押栈形态上：基址是全局指针的**值**（重算要再读一次），右值是 call
/// （认不得的形态一律当会改写那个名字）—— 两道门任意一道失守，本条立即变红。
#[test]
fn o2_unrecompute_addr_with_call_rhs_still_pushes() {
    let body = harness::dcc_fn_body_at(
        "p13-push-keep",
        "int *q;\nextern int f(void);\nvoid store(void) { *q = f(); }\n",
        "-O2",
        "store",
    );
    assert!(
        harness::non_frame_push_pop(&body) >= 2,
        "-O2: 不可重算地址 + call 右值的标量写被当成可以省押栈（两道保守门失守）：\n{body}"
    );
}
