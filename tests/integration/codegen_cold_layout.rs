// included from tests/integration.rs — not a standalone cargo target

// ===== 冷格外置（`DCC_COLD_OUTLINE`）的执行级与结构级判据 =====
//
// 这一族用例守的是「把一条冷边搬到函数尾」这个动作的语义不变性。发射器是单遍直写，
// 外置只能「先记下分支体、到函数尾再发」，于是有四类只有跑起来才会暴露的错：
//
//   1. `break`/`continue` 改绑。体 里的 `break` 要解到**它在 AST 里的那个**循环/switch，
//      而发射时用的是 `loop_break`/`break_targets` 现场栈；到函数尾再发时那个栈已经是
//      函数级的 ⇒ `break` 会跳到外层甚至悬空。修法是入队时快照三个栈、发完还原
//      （`emit/stmt.rs` 的 `flush_cold_arms`）。
//
//   2. 极性写反。原地发射是「条件为假则跳过体」；外置后必须改成「条件为真则去冷块」。
//      忘反极性的后果是把**热**侧搬走而冷侧原地不动 —— 数值照对，只有性能反向，
//      所以必须用结构判据（对象字节变了）而不是只看结果。
//
//   3. 不该外置的体被外置。`__attribute__((cleanup))` 的清理调用由 `end_cleanup_scope`
//      在**当前位置**发出，清理栈按发射顺序 LIFO；VLA 依赖「块结束时 rsp 已复原」；
//      内联 asm 的数字标签与 C++ 受调用的 LSDA 区间都按发射位置结算。这三类由
//      `cold_outline_blocked` 挡下，判据是「旋钮动了但这里没动」。
//
//   4. `!__builtin_expect(x, 0)` 的 then 侧是**热**侧（x 很可能为真）。把提示的否定形态
//      也当冷侧外置，等于把常走的那条边挪到函数尾并让它每次多一跳 —— 与目标完全相反。
//
// 结构判据为什么用「对象字节」而不是「函数大小」：外置一条冷边是一个 `jcc` 加一个
// `jmp` 换了位置，`.text` 总长很可能一字不变；只有字节序列一定变。
//
// 夹具为什么一律带一个永不为真的计算转移（`cg_wrap`）：实测 `__builtin_expect` 只在
// **跳过 AST/SSA pass 的函数**里能活到 codegen。普通函数走完整优化通道，提示在到达
// 发射器之前就被剥平（一条都不外置）；而 `luaV_execute` / `sqlite3VdbeExec` 这类
// computed-goto 函数整条跳过 pass，提示保留。本方案的目标恰好是那两个解释器，
// 所以夹具照目标的形状写，而不是假装普通函数也已生效（那是射程声明，不是事实）。

/// 强制本函数落进「非结构化通道」的序章：一张跳表 + 一条永不为真的 `goto *表[0]`。
const CG_PRE: &str = r#"
    extern void exit(int);
    static const void *cg_dt[] = {&&cgA, &&cgB};
    int cg_seed;
"#;

/// 把一段计算 `return` 值的函数体包成「带计算转移的 main」。
fn cg_wrap(body: &str) -> String {
    format!(
        r#"
{CG_PRE}
    int main(void) {{
        if (cg_seed < 0) {{ goto *cg_dt[0]; }}
        cgA: ;
{body}
        cgB: ;
        return 0;
    }}
"#
    )
}

/// 冷格外置的公共夹具：同一段源码在旋钮关/开两种档位下分别编译并运行。
///
/// 返回 `(rc_off, rc_on, out_off, out_on, bytes_off, bytes_on)`。值判据要求前四者两两
/// 相等；机制判据由调用方对后两者主张 `!=`（该动的动了）或 `==`（不该动的没动）。
fn cold_outline_pair(name: &str, source: &str) -> (i32, i32, String, String, Vec<u8>, Vec<u8>) {
    let on: &[(&str, &str)] = &[("DCC_COLD_OUTLINE", "1")];
    let (out_off, exe_off) = run_compile_and_run_env(name, source, &["-O2"], &[]);
    let (out_on, exe_on) = run_compile_and_run_env(name, source, &["-O2"], on);
    assert!(
        out_off.status.success() && out_on.status.success(),
        "编译失败 off={} on={}",
        String::from_utf8_lossy(&out_off.stderr),
        String::from_utf8_lossy(&out_on.stderr)
    );
    let (o1, b1) = run_compile_object_env(name, source, "-O2", &[]);
    let (o2, b2) = run_compile_object_env(name, source, "-O2", on);
    assert!(o1.status.success() && o2.status.success(), "对象编译失败");
    // 夹具用 `exit(计算值)` 回传结果 ⇒ 非零退出码是正常现象，不能拿 `status.success()`
    // 当「跑对了」；只拒接硬崩（拿不到退出码 = 被信号打死）。
    let rc = |p: &std::path::PathBuf| {
        let o = Command::new(p).output().expect("run fixture");
        assert!(
            o.status.code().is_some(),
            "夹具被信号打死: {:?} {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr)
        );
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
        )
    };
    let (rc_off, out_off) = rc(&exe_off);
    let (rc_on, out_on) = rc(&exe_on);
    let _ = fs::remove_file(&exe_off);
    let _ = fs::remove_file(&exe_on);
    (rc_off, rc_on, out_off, out_on, b1, b2)
}

/// 值判据：两档必须给出完全相同的退出码与 stdout。
fn assert_cold_same_value(t: &(i32, i32, String, String, Vec<u8>, Vec<u8>), what: &str) {
    let (rc_off, rc_on, out_off, out_on) = (t.0, t.1, t.2.clone(), t.3.clone());
    assert_eq!(rc_off, rc_on, "{what}: 旋钮开后的退出码与关时不同（{rc_off} vs {rc_on}）");
    assert_eq!(out_off, out_on, "{what}: 旋钮开后的 stdout 与关时不同");
}

/// `break` 在冷侧体内：外置后仍必须跳出**本层**循环，且累计值不变。
///
/// 这条同时是「旋钮真的动了」的机制判据：守卫必须被搬走，对象字节一定要变。
/// 字节没变就说明判据在空跑（本仓已有过空判据的教训）。
#[test]
fn cold_outline_break_inside_guard_still_breaks_this_loop() {
    let src = cg_wrap(
        r"        int s = 0, n = 0;
        for (int i = 0; i < 10; i++) {
            if (__builtin_expect(i == 2, 0)) { break; }
            s += i; n++;
        }
        exit(s * 3 + n);",
    );
    let t = cold_outline_pair("cold-break", &src);
    assert_cold_same_value(&t, "break 在冷侧体内");
    assert_eq!(t.0, 5, "夹具本身的期望值就不对（应为 (0+1)*3+2）");
    assert_ne!(t.4, t.5, "守卫没被外置 ⇒ 本条判据在空跑");
}

/// `continue` 与守卫之后仍有代码：外置不得让 fall-through 越过后面的语句。
#[test]
fn cold_outline_fallthrough_reaches_later_statements() {
    let src = cg_wrap(
        r"        int s = 0;
        for (int i = 0; i < 6; i++) {
            if (__builtin_expect(i == 3, 0)) { continue; }
            s += i;
            if (__builtin_expect(i == 5, 0)) { s += 100; }
            s += 1;              // 守卫之后还有语句：外置后 fall-through 必须回到这里
        }
        exit(s);   // (0+1+2+4+5) + 5 次 +1 + 100 = 117",
    );
    let t = cold_outline_pair("cold-fall", &src);
    assert_cold_same_value(&t, "守卫之后仍有语句");
    assert_eq!(t.0, 117, "夹具本身的期望值就不对");
    assert_ne!(t.4, t.5, "没有外置发生");
}

/// 冷侧体内有标签且被外部 `goto` 命中：标签仍必须可解析、且只被那个 `goto` 进入。
///
/// 这是 `if (0) { L: ... }` 那一族的加强版：块被搬到函数尾后，`goto L` 的落点必须
/// 跟着搬到新位置，而 fall-through 绝不能顺落进体 里（否则等价于多执行一次冷体）。
#[test]
fn cold_outline_goto_into_body_label_resolves_after_move() {
    let src = cg_wrap(
        r"        int s = 0;
        for (int i = 0; i < 4; i++) {
            if (__builtin_expect(i == 9, 0)) {   // 永不成立，但体 里有标签
            again:
                s += 7;
                if (s < 21) { goto again; }
                continue;
            }
            s += i;
            if (i == 2) { goto again; }          // 从守卫外跳进被外置的体
        }
        exit(s);",
    );
    let t = cold_outline_pair("cold-goto", &src);
    assert_cold_same_value(&t, "goto 跳进被外置的体");
    // i=0:+0  i=1:+1  i=2:+2 后 goto again ⇒ +7,+7,+7(=24) 再 continue  i=3:+3 ⇒ 27
    assert_eq!(t.0, 27, "夹具本身的期望值不对");
    assert_ne!(t.4, t.5, "没有外置发生");
}

/// `return` 在冷侧体内：体 末尾不得再多发一条跳回汇合点的 `jmp`。
#[test]
fn cold_outline_return_inside_body_does_not_fall_back() {
    let src = r#"
        extern void exit(int);
        static const void *cg_dt[] = {&&cgA, &&cgB};
        int cg_seed;
        int f(int k) {
            if (cg_seed < 0) { goto *cg_dt[0]; }   // 只为把本函数压进非结构化通道
            cgA: ;
            for (int i = 0; i < 5; i++) {
                if (__builtin_expect(k == i, 0)) { return i + 10; }
            }
            cgB: ;
            return 99;
        }
        int main(void) { exit(f(3) + f(9) - 9); }   // 13 + 99 - 9 = 103
    "#;
    let t = cold_outline_pair("cold-ret", src);
    assert_cold_same_value(&t, "return 在冷侧体内");
    assert_eq!(t.0, 103, "夹具本身的期望值不对");
}

/// 嵌套两层循环：冷侧体里的 `break` 只能跳出**最近**那一层。
///
/// 快照判据的真实考点：`break_targets` 是栈，函数尾发射时栈已经空了。少了快照，
/// 这条会跳到外层循环的出口（实测同款症状是外层提前退出 ⇒ r 偏小）。
#[test]
fn cold_outline_nested_loop_break_binds_innermost() {
    let src = cg_wrap(
        r"        int r = 0;
        for (int i = 0; i < 4; i++) {
            for (int j = 0; j < 4; j++) {
                if (__builtin_expect(j == 2, 0)) { break; }   // 只出内层
                r += 1;
            }
            r += 10;
        }
        exit(r);",
    );
    let t = cold_outline_pair("cold-nest", &src);
    assert_cold_same_value(&t, "嵌套循环的 break 绑定");
    assert_eq!(t.0, 48, "夹具本身的期望值不对（每圈 2+10，共 4 圈）");
    assert_ne!(t.4, t.5, "没有外置发生");
}

/// switch 的 case 里的冷侧体：`break` 必须出 switch 而不是出外层循环。
#[test]
fn cold_outline_switch_case_break_binds_switch_not_loop() {
    let src = cg_wrap(
        r"        int r = 0;
        for (int i = 0; i < 3; i++) {
            switch (i) {
                case 1:
                    if (__builtin_expect(i, 0)) { r += 5; break; }   // 出 switch
                    r += 1;
                    break;
                default:
                    r += 2;
            }
            r += 20;
        }
        exit(r);   // i=0: 2+20; i=1: 5+20; i=2: 2+20 ⇒ 69（留在 256 内，退出码取低 8 位）",
    );
    let t = cold_outline_pair("cold-switch", &src);
    assert_cold_same_value(&t, "case 里的 break 绑定");
    assert_eq!(t.0, 69, "夹具本身的期望值不对");
}

/// 提示的**否定**形态：`!unlikely(x)` 的 then 侧是热侧，一律不得外置。
///
/// 这条只有结构判据能守住：把热侧搬走时代码照样能跑对，只有性能反向 —— 正是本方案
/// 要防的「表上变好、墙钟变差」的镜像形态（表上甚至看不出任何变化）。
#[test]
fn cold_outline_skips_negated_hint_because_then_is_hot() {
    let src = cg_wrap(
        r"        int s = 0;
        for (int i = 0; i < 20; i++) {
            if (!__builtin_expect(i % 7 == 0, 0)) { s += i; }   // 几乎总为真 ⇒ 热侧
            else { s -= 1; }
        }
        exit(s % 251);",
    );
    let t = cold_outline_pair("cold-neg", &src);
    assert_cold_same_value(&t, "否定形态的提示");
    assert_eq!(t.4, t.5, "否定形态被外置了 ⇒ 热侧被搬走");
}

/// 冷侧体里有 `__attribute__((cleanup))` 变量：不得外置，且清理仍要按序执行。
#[test]
fn cold_outline_blocked_by_cleanup_attribute() {
    let src = r#"
        extern void exit(int);
        static const void *cg_dt[] = {&&cgA, &&cgB};
        int cg_seed, hits;
        void rel(int *p) { hits += *p; }
        int main(void) {
            if (cg_seed < 0) { goto *cg_dt[0]; }
            cgA: ;
            for (int i = 0; i < 3; i++) {
                if (__builtin_expect(i == 1, 0)) {
                    int v __attribute__((cleanup(rel))) = 11;
                    hits += 1;
                    (void)v;
                }
                hits += 7;
            }
            cgB: ;
            exit(hits);    // i=0: 7; i=1: +1 +7 +11(清理); i=2: +7 ⇒ 33
        }
    "#;
    let t = cold_outline_pair("cold-cleanup", src);
    assert_cold_same_value(&t, "cleanup 属性的清理顺序");
    assert_eq!(t.0, 33, "夹具本身的期望值不对");
    assert_eq!(t.4, t.5, "含 cleanup 的体被外置了 ⇒ 析构顺序会变");
}

/// 冷侧体里有 VLA：不得外置（`emit_vla_restore` 依赖块结束时的 rsp 复原顺序）。
#[test]
fn cold_outline_blocked_by_vla_in_body() {
    let src = cg_wrap(
        r"        int n = 4;
        int s = 0;
        for (int i = 0; i < 3; i++) {
            if (__builtin_expect(i == 1, 0)) {
                int a[n];
                for (int k = 0; k < n; k++) { a[k] = k; }
                s += a[2];
            }
            s += 5;
        }
        exit(s);   // 5 + (2+5) + 5 = 17",
    );
    let t = cold_outline_pair("cold-vla", &src);
    assert_cold_same_value(&t, "VLA 冷侧体");
    assert_eq!(t.0, 17, "夹具本身的期望值不对");
    assert_eq!(t.4, t.5, "含 VLA 的体被外置了");
}

/// 冷侧体里有内联 asm：不得外置（asm 的数字标签与行号都在模板内按发射位置解析）。
#[test]
fn cold_outline_blocked_by_inline_asm_in_body() {
    let src = cg_wrap(
        r#"        int s = 0;
        for (int i = 0; i < 3; i++) {
            if (__builtin_expect(i == 1, 0)) {
                __asm__ volatile("nop");
                s += 3;
            }
            s += 1;
        }
        exit(s);   // 1 + (3+1) + 1 = 6"#,
    );
    let t = cold_outline_pair("cold-asm", &src);
    assert_cold_same_value(&t, "内联 asm 冷侧体");
    assert_eq!(t.0, 6, "夹具本身的期望值不对");
    assert_eq!(t.4, t.5, "含 asm 的体被外置了");
}

/// 默认位必须一动不动：不带旋钮时与「显式关」逐字节相同。
///
/// 这条是整个冷格通道的零漂移门 —— 一旦默认值被误开，所有历史产物判据当场失去意义。
#[test]
fn cold_outline_is_off_by_default() {
    let src = cg_wrap(
        r"        int s = 0;
        for (int i = 0; i < 8; i++) {
            if (__builtin_expect(i == 7, 0)) { s += 50; }
            s += i;
        }
        exit(s);",
    );
    let (o1, b1) = run_compile_object_env("cold-default-a", &src, "-O2", &[]);
    let (o2, b2) = run_compile_object_env("cold-default-b", &src, "-O2", &[("DCC_COLD_OUTLINE", "0")]);
    assert!(o1.status.success() && o2.status.success(), "编译失败");
    assert_eq!(b1, b2, "默认位与显式关不一致 ⇒ 旋钮在默认路径上漏生效");
    let (_o3, b3) = run_compile_object_env("cold-default-c", &src, "-O2", &[("DCC_COLD_OUTLINE", "1")]);
    assert_ne!(b1, b3, "旋钮开了产物却没变 ⇒ 冷格通道根本没接上");
}
