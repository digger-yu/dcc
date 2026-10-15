// included from tests/integration.rs — not a standalone cargo target

// ===== P16 真轮转寄存器（Phase 6.3）的接受、产物排布与反向对照 =====
//
// 这条主题钉的是「保守拒收被解除之后，收益真的到了产物，而且没有变成错值」。两类
// 失败各自都骗过测试，所以三段判据缺一不可：
//
//   (a) **只测执行值**：pass 退化成一扇永不打开的门时，所有执行对照照样全绿
//       （这条教训：接入点在管线末尾时，开关两边的 -O2
//       汇编 sha256 完全相同）。所以必须有「`DCC_PIPE_DUMP` 的接受读数」+「反汇编里
//       出现了只有轮转才排得出来的形状」两条产物级断言。
//   (b) **只测接受读数**：计划排出来了、值却是错的（chain6_10 当年 -O2 得 7006 而真值
//       5005，§7.6）。所以同一夹具必须逐档与 gcc 对照。
//
// 轮转的产物指纹（判据在这里写死，测试里就是这么算的）：把 `rot_load4` 的函数体按
// **回边**切成 `prologue / kernel / epilogue` 三段，数每段里对 `idx` 的下标载入次数。
// 未流水时是 `0 / 4 / 0`（载入全在原循环体内）；真轮转 depth=2 时是 `6 / 4 / 2`
// —— 进循环**之前**就已经算完了 1.5 轮的下标链，而那些值必须在循环里被逐轮消费，
// 这只有在「同一基名的两份值同时有位置」时才可能成立。塌缩模型（depth≤1）排不出
// 这个形状，因为它一个基名只有一个槽；`DCC_SSA_OFF=rot` 的实测读数正是 `0 / 4 / 0`。
//
// 最后一条测试钉的是从这条链里挖出来的一条**既存静默误编译**（不是本轮引入的，
// 本轮的基线二进制同样错）：两级依赖载入链 `int a=arr[i]&7; int b=arr[a]&7;` 在 -O2
// 得 372 而 gcc 四档都是 375。根因在 `src/ssa/inline_temps.rs` —— `use_count` 与
// `find_use_site` 只扫指令与 terminator、不看 phi 的 incoming，于是「唯一读者是头块
// 那个轮转 phi」的合成临时值被判定「无人读 ⇒ 定义可以内联掉」，而 `rebuild_cycle`
// 恰恰把那个 phi 的 dest **别名**到该 incoming（那次定义就是槽的初始化）⇒ 槽从未被写
// ⇒ 静默读未初始化。判据取它的产物形态：沿发射序扫函数体，任何栈槽在**第一次被读
// 之前必须已被写过**。
//
// 主题子文件内不得写行首 `fn`（tests/README.md §2.1 规则 2；
// scripts/gen_coverage_index.sh 的 STRUCT-HELPER 要求行首 `fn` 数与测试属性数相等；
// 注：那条自检是**子串**计数，所以本注释里也不得写出那个属性字面串，否则会多出
// 一条幽灵用例。所以下面的解析器都就近嵌套在测试体里，重复一处换不污染命名面。

/// P16 轮转夹具的四档执行对照（期望值 375/467/403/53/475027 全部取自 gcc 实测，
/// 见夹具头部注释）。
#[test]
fn p16_rotation_fixture_matches_gcc_at_all_levels() {
    harness::assert_all_levels_match_gcc(
        "p16-rotation",
        include_str!("../fixtures/opt/ssa_pipe_rotation.c"),
        "真轮转寄存器改变了循环语义（轮转格的播种 / 行尾拷贝链 / 跳轮读三者之一错位）",
    );
}

/// 正向判据（读数一）：被旧判据拒收的 depth=2 循环现在**确实被接受**，且格子表非空。
///
/// `rot_cells` 是 `InstancePlan::cells` 的直接投影，所以这条断言钉的是「位置粒度从
/// 基名下到了版本」这件事真的进了计划，而不是只在源码里存在。
#[test]
fn p16_rotation_accepts_depth_two_loops_with_own_cells() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    let (_asm, dump) = harness::compile_asm_text_with_env(
        "p16-rot-accept",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1")],
    );

    // 每个接受形状应有的格子（实测 `DCC_PIPE_DUMP=1` 的 rot_cells 行）。
    let expect: [(&str, [&str; 4]); 2] = [
        ("rot_load4", ["b_s1", "_tidx_9_s1", "s_s1", "i_s1"]),
        ("rot_load5", ["_tbin_6_s1", "_tidx_11_s1", "s_s1", "i_s1"]),
    ];
    for (func, cells) in expect {
        assert!(
            dump.contains(&format!("fn {func} header=")),
            "{func}：没有接受记录 ⇒ depth>1 的旧保守判据还没解除（本条正是 Phase 6.3 的正面判据）\ndump={dump}"
        );
        let line = dump
            .lines()
            .skip_while(|l| !l.starts_with(&format!("fn {func} header=")))
            .find_map(|l| {
                let t = l.trim_start();
                if t.starts_with("rot_cells=") { Some(t) } else { None }
            })
            .unwrap_or_else(|| panic!("{func}：接受了却没有 rot_cells 读数（格子表的可观测面丢失）\ndump={dump}"));
        assert!(
            line.starts_with("rot_cells=[") && line.contains("lag1->"),
            "{func}：格子表应为真轮转而不是 `none(基名塌缩)`，实际「{line}」"
        );
        for c in cells {
            assert!(
                line.contains(&format!("->{c}")),
                "{func}：格子表里应有 `{c}` 这个独立家，实际「{line}」"
            );
        }
    }

    // 老接受形状不许抖：depth=1 的算术链仍走模型 1（格子表为空 = 基名塌缩）。
    assert!(
        dump.contains("fn rot_flat header=") && dump.contains("rot_cells=none(基名塌缩)"),
        "depth=1 的既有接受形状应继续走塌缩模型（真轮转只在模型 1 失败时才试）\ndump={dump}"
    );
    // 代价判据仍然成立，而且**必须报真原因**：`depth>1` 在轮转路径上已不是拦路虎，
    // 若还报旧文案，拒因就指向一条已经不存在的限制。
    let short = dump
        .lines()
        .find(|l| l.contains("[P16] rot_short#for.body 未流水"))
        .unwrap_or_else(|| panic!("rot_short（trip=4）应仍被拒：代价不划算\n{dump}"));
    assert!(
        short.contains("trip 太短"),
        "rot_short 的拒因应为真原因「trip 太短」，实际「{short}」"
    );
    assert!(
        !short.contains("需要超过一级轮转寄存器"),
        "rot_short 的拒因仍指向已被解除的旧限制（拒因不诚实）：「{short}」"
    );
}

/// 把函数体按最宽的回边切成三段，并数每段里对 `idx` 的下标载入次数。
/// 以宏的形式给出，是因为两个测试都要用它，而行首 `fn` 会破坏 STRUCT-HELPER 判据。
macro_rules! p16_rot_split_loads {
    ($body:expr) => {{
        let body: &str = $body;
        let lines: Vec<&str> = body.lines().collect();
        let mut def: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, l) in lines.iter().enumerate() {
            if let Some(head) = l.strip_suffix(':') {
                if !l.starts_with('\t') && !l.starts_with(' ') {
                    def.insert(head.to_string(), i);
                }
            }
        }
        let mut best: Option<(usize, usize)> = None;
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim();
            let head = t.split_whitespace().next().unwrap_or("");
            let jumps = head == "jmp" || head.starts_with('j');
            if !jumps {
                continue;
            }
            let target = match t.split_whitespace().last() {
                Some(x) if x.starts_with('.') => x,
                _ => continue,
            };
            if let Some(&d) = def.get(target) {
                if d < i && best.map_or(true, |(bd, bi)| i - bd > bi - bd) {
                    best = Some((d, i));
                }
            }
        }
        let (kd, kr) = best.expect("函数体里应能找到回边（循环还在）");
        let hits = |r: std::ops::Range<usize>| -> usize {
            // 指纹取 `lea idx(%rip)`：每次下标载入恰好伴一条（寻址寄存器怎么分配不影响
            // 这个计数，而 `movsxd (%r11,...)` 那种写法会把后端的寄存器选择当判据）。
            lines[r].iter().filter(|l| l.contains("idx(%rip)")).count()
        };
        (hits(0..kd), hits(kd..kr + 1), hits(kr + 1..lines.len()))
    }};
}

/// 正向判据（读数二，产物级）：反汇编里真的出现了只有轮转才排得出的三段形状。
#[test]
fn p16_rotation_emits_warmup_kernel_and_drain() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    let (asm, dump) = harness::compile_asm_text_with_env(
        "p16-rot-layout",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1")],
    );
    assert!(dump.contains("fn rot_load4 header="), "前置：接受记录缺失\n{dump}");

    let body = harness::extract_function_body(&asm, "rot_load4");
    assert!(!body.is_empty(), "取不到 rot_load4 的函数体");
    let (pre, kernel, post) = p16_rot_split_loads!(&body);
    assert_eq!(kernel, 4, "kernel 每行应含 4 级载入链：{body}");
    assert!(
        pre >= 4,
        "prologue 必须在进循环前就预热出整轮载入（只有轮转格才接得住这些值）：pre={pre} kernel={kernel} post={post}"
    );
    assert!(
        post >= 1,
        "epilogue 必须排空尾轮（depth=2 ⇒ 循环外还有实例没跑）：pre={pre} kernel={kernel} post={post}"
    );
}

/// 反向对照（一）：关掉真轮转（`DCC_SSA_OFF=rot`）必须**逐字**回到旧行为 —— 旧文案
/// 「需要超过一级轮转寄存器（本层不做）」，且产物里循环体外一次载入都没有。
#[test]
fn p16_rotation_off_reverts_to_the_old_conservative_rejection() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    let (asm, dump) = harness::compile_asm_text_with_env(
        "p16-rot-off",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1"), ("DCC_SSA_OFF", "rot")],
    );

    for func in ["rot_load4", "rot_load5", "rot_clash", "rot_short"] {
        let line = dump
            .lines()
            .find(|l| l.contains(&format!("[P16] {func}#for.body 未流水")))
            .unwrap_or_else(|| panic!("{func}：关掉轮转后必须有拒因记录（旧行为里它被 depth>1 拒）\n{dump}"));
        assert!(
            line.contains("需要超过一级轮转寄存器（本层不做）"),
            "{func}：`DCC_SSA_OFF=rot` 应逐字退回旧判据文案，实际「{line}」"
        );
        assert!(
            !dump.contains(&format!("fn {func} header=")),
            "{func}：关掉轮转后仍被接受 ⇒ 反向对照失效"
        );
    }
    // depth=1 的老接受形状在 rot 关闭时依旧被收：模型 2 只在模型 1 失败时才试。
    assert!(
        dump.contains("fn rot_flat header="),
        "rot_flat（depth=1）不该因关闭轮转而失去既有的流水\n{dump}"
    );

    // 产物侧：没有轮转格就没有 prologue 预热 —— 回边覆盖范围之外对 idx 的载入必须为 0。
    let body = harness::extract_function_body(&asm, "rot_load4");
    let (pre, _kernel, post) = p16_rot_split_loads!(&body);
    assert_eq!(
        pre + post,
        0,
        "`DCC_SSA_OFF=rot` 下循环体外出现了 {} 次 idx 载入 ⇒ 旧行为里也有轮转，反向对照不成立",
        pre + post
    );
}

/// 反向对照（二）：关掉整条 P16（`DCC_SSA_OFF=pipe`）给出的是**更早**的行为 —— 连接受
/// 记录都没有。两条开关的读数不同，正好把「轮转层」与「流水层」的边界钉住。
#[test]
fn p16_pipe_off_gives_the_pre_pipelining_product() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    let (_asm, dump) = harness::compile_asm_text_with_env(
        "p16-rot-pipeoff",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1"), ("DCC_SSA_OFF", "pipe")],
    );
    for func in ["rot_load4", "rot_load5", "rot_clash", "rot_short", "rot_flat"] {
        assert!(
            !dump.contains(&format!("fn {func} header=")),
            "{func}：`DCC_SSA_OFF=pipe` 下不应有任何流水接受记录（那是本 pass 的总开关）\n{dump}"
        );
    }
    // 默认路径必须与「关整条 pipe」不同：证明轮转层真的到达了产物。
    let on = harness::dcc_asm_with_pipe("p16-rot-prod", src, None);
    let off = harness::dcc_asm_with_pipe("p16-rot-prod", src, Some("pipe"));
    assert_ne!(
        harness::asm_body(&on),
        harness::asm_body(&off),
        "流水（含轮转层）接进管线而产物逐字节相同 ⇒ pass 空转"
    );
}

/// 分裂占名检查必须被遵守：用户自己已有局部 `i_s1`，而归纳变量 `i` 的格子恰好想要
/// `i_s1` ⇒ 格子必须避让到 `i_s2`。`LocMap::reserve_split_name` 是全仓唯一的命名分配器
/// （版本分裂与轮转格共用它），流水侧不许有第二套判据 —— 这条测试就是那个「不许」的
/// 机器判据：绕过它就会拿到 `i_s1`，两个局部同名，产物立刻错值。
#[test]
fn p16_rotation_cell_names_respect_existing_user_locals() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    let (_asm, dump) = harness::compile_asm_text_with_env(
        "p16-rot-clash",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1")],
    );
    let line = dump
        .lines()
        .skip_while(|l| !l.starts_with("fn rot_clash header="))
        .find_map(|l| {
            let t = l.trim_start();
            if t.starts_with("rot_cells=") { Some(t) } else { None }
        })
        .expect("rot_clash 被接受了却没有 rot_cells 读数（可观测面丢失）");
    assert!(
        line.contains("->i_s2"),
        "用户已占用 `i_s1` ⇒ `i` 的格子必须避让到 `i_s2`，实际「{line}」"
    );
    assert!(
        !line.contains("->i_s1\"") && !line.contains("->i_s1]"),
        "格子拿到了用户已有名 `i_s1` ⇒ 分裂占名检查被绕过（两个局部同名）：「{line}」"
    );
}

/// 准入闸门的接受数必须因本轮而**变大**，而且增量正好是那个轮转形状：
/// `tests/fixtures/opt/ssa_pipe_chain.c` 的 `header=` 记录数 2 → 3（
/// `scripts/check_pipe_structure.sh` 末行的「接受」读数随之 4 → 6，它按 `^fn ` 行计，
/// 每个接受循环出两行）。关掉轮转必须退回 2。
#[test]
fn p16_gate_fixture_acceptance_count_grows_by_the_rotated_loop() {
    let src = include_str!("../fixtures/opt/ssa_pipe_chain.c");
    let (_asm, on) = harness::compile_asm_text_with_env(
        "p16-chain-on",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1")],
    );
    let (_asm, off) = harness::compile_asm_text_with_env(
        "p16-chain-off",
        src,
        &["-O2", "-std=gnu2x"],
        &[("DCC_PIPE_DUMP", "1"), ("DCC_SSA_OFF", "rot")],
    );
    let accepted = |d: &str| {
        d.lines()
            .filter(|l| l.starts_with("fn ") && l.contains("header="))
            .count()
    };
    assert_eq!(
        accepted(&off),
        2,
        "旧行为里 ssa_pipe_chain.c 只该收 ch 与 chain6_10（反向对照基线）\n{off}"
    );
    assert_eq!(
        accepted(&on),
        3,
        "真轮转应把 chain_load4 也收进来（接受数 2 → 3）\n{on}"
    );
    assert!(
        on.contains("fn chain_load4 header=")
            && off.contains("[P16] chain_load4#for.body 未流水：需要超过一级轮转寄存器（本层不做）"),
        "chain_load4 必须是「旧拒 → 新收」的那一条，否则接受数的变化与轮转无关"
    );
    // 四条既有拒因不得因本轮改动而漂移（check_pipe_structure.sh 硬编码同一条清单）。
    for (func, why) in [
        ("ch_store", "归纳变量初值不是区域外的唯一常量定义"),
        ("red", "无跳迭代重叠"),
        ("chain6_6", "trip 太短"),
        ("chain5_24", "无跳迭代重叠"),
    ] {
        let line = on
            .lines()
            .find(|l| l.contains(&format!("[P16] {func}#for.body 未流水")))
            .unwrap_or_else(|| panic!("{func}：拒因记录消失（准入判定变得不可解释）\n{on}"));
        assert!(
            line.contains(why),
            "{func}：拒因漂移，期望含「{why}」，实际「{line}」"
        );
    }
}

/// 从轮转里挖出来的那条既存缺陷的产物级判据：**任何栈槽在被第一次读之前必须已被写过**。
///
/// 旧行为下 `rot_chain2`/`rot_load4` 的 prologue 少一条把 `_tbin = a & 7` 落进槽的 store（那条定义
/// 被当成「无人读的内联临时值」丢掉，而它唯一的读者是头块那个轮转 phi），于是 kernel 读
/// 那个槽拿到未初始化的 0：-O2 得 372 而 gcc 四档都是 375。这条判据不依赖具体变量名，
/// 所以同族的其余形状（`rot_load4`、`rot_load5`、`rot_clash`）也一并护住。
///
/// 反向实验做过：把 `inline_window_safe` 里那条守卫改回无条件 `return Some(info)` 后，
/// 同一份夹具在 -O2 报 372（实测），而本测试的 `rot_chain2` 那一例变红。
#[test]
fn p16_rotation_every_slot_is_written_before_its_first_read() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    for func in ["rot_chain2", "rot_load4", "rot_load5", "rot_clash"] {
        let (asm, dump) = harness::compile_asm_text_with_env(
            "p16-rot-init",
            src,
            &["-O2", "-std=gnu2x"],
            &[("DCC_PIPE_DUMP", "1")],
        );
        assert!(
            dump.contains(&format!("fn {func} header=")),
            "前置：{func} 未被接受\n{dump}"
        );
        let body = harness::extract_function_body(&asm, func);
        assert!(!body.is_empty(), "取不到 {func} 的函数体");

        // AT&T 语法 `op 源, 目的`：最后一个操作数是内存 ⇒ 写；其余位置出现内存 ⇒ 读。
        // `cmp`/`test` 例外（它们的目的也不写）。`lea` 的目的只会是寄存器。
        let offset_of = |s: &str| -> Option<i64> {
            let i = s.find("(%r13)")?;
            let digits: String = s[..i].chars().rev().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            digits.chars().rev().collect::<String>().parse::<i64>().ok().map(|v| -v)
        };
        let mut written: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
        let mut offenders: Vec<i64> = Vec::new();
        for line in body.lines() {
            let t = line.trim();
            if !t.contains("(%r13)") {
                continue;
            }
            let head = t.split_whitespace().next().unwrap_or("");
            let pure_read = head.starts_with("cmp") || head.starts_with("test");
            let parts: Vec<&str> = t.split(", ").collect();
            let last = parts.len() - 1;
            for (i, p) in parts.iter().enumerate() {
                let Some(o) = offset_of(p) else { continue };
                let is_dst = i == last && parts.len() > 1;
                if is_dst && !pure_read {
                    written.insert(o);
                } else if !written.contains(&o) {
                    offenders.push(o);
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "{func}：有 {} 个栈槽在未被写过前就被读（静默读未初始化），槽偏移={:?}（相对 %r13）",
            offenders.len(),
            offenders
        );
    }
}

/// 期望值的字面断言：让「少算一圈」那类回归在失败信息里直接可读，而不是只看到
/// 「与 gcc 不同」。数字全部来自 gcc 实测（夹具头部注释里同一份）。
#[test]
fn p16_rotation_runtime_values_are_the_gcc_ones() {
    let src = include_str!("../fixtures/opt/ssa_pipe_rotation.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            harness::run_gcc_cross_check_opt("p16-rotation-values", src, &[flag]),
            0,
            "{flag}: 轮转后的 rot_* 夹具与 gcc 不一致（期望 rot_chain2=375 / rot_load4=375 / rot_load5=467 / rot_clash=403 / rot_short=53 / rot_flat=475027）"
        );
    }
}
