// included from tests/integration.rs — not a standalone cargo target

// ===== 帧槽事实 / 屏障重划 / 非局部控制流护栏 的「能变红」判据（方案 §5）=====
//
// 这一族对应方案 §5 点名的五条用例。每条都把「红法」写在头上的注释里，因为一条永远
// 不会红的判据比没有判据更糟：它会让后面的轮次以为自己被保护过。
//
//   1. `raw_facts_survive_after_raw_insn`（S1 facts 层）
//      红法：删掉 `observe` 里 `Raw` 那条臂 ⇒ 语料的帧载入次数回升 ⇒ 本条变红。
//   2. `slot_fact_survives_call_equivalence`（S2 A1）
//      红法：让 A1 无条件保留帧槽事实（不看护栏、不杀 caller-saved 号）⇒ 四档结果
//      不再一致 ⇒ 本条变红。单位级的两臂差异由 `blockopt::tests::a1_*` 三条钉住。
//   3. `slot_fact_crosses_join_label`（S2 A2 的落空继承那一臂）
//   4. `slot_fact_not_kept_at_loop_head`（A2 的反面：循环头必须重载）
//      3 与 4 必须同时存在：只写 3，实现会顺手把所有标签都继承掉。
//   5. `nonlocal_home_longjmp_ext`（§4.1）
//      红法：把 `__builtin_setjmp`/`__builtin_longjmp` 从 `RETURNS_TWICE_NAMES` 删掉
//      ⇒ 值判据与结构判据同时变红。
//
// 为什么判据一律写成「两臂对比」而不是「一臂 + 数一数」：方案里每一条放宽都声称
// 「关掉它产物逐字节不变」，只有同源码两臂的差能同时证明「开了有用」与「关了没漂」。
//
// 为什么第 1 条用仓库内语料而不是手写小夹具：实测三次尝试都失败 —— 十几行的夹具里
// `keep` 要么整个活在寄存器里（根本没有帧槽事实可转发），要么 store/load 对之间夹的
// 是 label 而不是 raw 指令。S1 的收益形状属于「大函数 + 多槽 + 中间夹 raw」，
// `bootstrap_selftest.c`（1.9k 条、127 次帧载入）是本仓里能自洽跑通的那个形状。

/// 规则位图的具名常量（与 `blockopt.rs` 的位号一一对应）。
///
/// 为什么在这里重述一遍而不是 import：位号是 `pub(crate)` 的后端内部事实，而本文件
/// 要的是「这一臂开/关」的**外部表达**。写成 0x2F 这种裸数字的话，位号一变整族判据
/// 会静默变成「两臂同形」—— 那正是假绿的形状。
const BASE_NO_RAW: &str = "0x0F"; // R_SEXT|R_COPY|R_SLOT|R_ZBITS
const DEF_WITH_RAW: &str = "0x2F"; // 上面再加 R_RAW（= 第十四轮之前的默认档）
/// 今天的默认档：除上面那些再加 `R_IMMSEXT`（1<<12，本轮补的那批 raw 扩展事实）。
/// 两个常数分开留着不是冗余：`0x2F` 是「本轮改动之前长什么样」的对照臂，而
/// 「量尺必须能单独关」本身就是本轮拆这个位的原因（写在 `R_IMMSEXT` 的注释里）。
const DEF_NOW: &str = "0x102F";
/// 三条臂的基准都取 `DEF_NOW`（今天的默认档）再叠自己那一位：基线与对照臂差**恰一位**
/// 才是「单独一条规则的读数」。基准里缺了 `R_IMMSEXT` 的话，A1/A2/A3 那三臂会连带
/// 把本轮新补的事实一起关掉，量出来的就不是那一条臂自己。
const A1_ON: &str = "0x112F"; // 默认档 + R_CALLSLOT
const A2_ON: &str = "0x182F"; // 默认档 + R_LABELJOIN
const A3_ON: &str = "0x142F"; // 默认档 + R_LRU

/// S1 的语料：仓库里最大的一份自包含 C 源（计算转移 + 多帧槽）。
const S1_CORPUS: &str = "tests/fixtures/c/bootstrap_selftest.c";

/// 数一份汇编里的**帧载入**次数。
///
/// 为什么不能用 `grep -c r13`：`mov %rax, -0x30(%r13)` 是存出，把它计进来会让
/// 「转发掉一次载入」这件事读数不动 —— 判据会对着真正的那一半失效。
fn frame_loads_in_asm(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with("mov")
                || l.starts_with("add")
                || l.starts_with("sub")
                || l.starts_with("cmp")
                || l.starts_with("test")
                || l.starts_with("lea")
        })
        // 载入 = 带 `(%r13)` 的那一侧在**逗号前**（源）。AT&T 是 `src, dst`。
        .filter(|l| match l.find(',') {
            Some(i) => l[..i].contains("(%r13"),
            None => false,
        })
        .count()
}

fn insn_count(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with('.')
                && !l.starts_with('#')
                && l.chars().next().map_or(false, |c| c.is_ascii_lowercase())
        })
        .count()
}

fn compile_s1_with(rules: &str) -> (String, String) {
    let src = std::fs::read_to_string(S1_CORPUS)
        .unwrap_or_else(|e| panic!("读语料 {S1_CORPUS} 失败：{e}"));
    compile_asm_text_with_env("s1-corpus", &src, &["-O2"], &[("DCC_BLOCKOPT_RULES", rules)])
}

/// 判据 1：一条只写目的号的 raw 指令，之后不得重载与之无关的槽。
#[test]
fn raw_facts_survive_after_raw_insn() {
    let (off, _) = compile_s1_with(BASE_NO_RAW);
    let (on, _) = compile_s1_with(DEF_WITH_RAW);
    let n_off = frame_loads_in_asm(&off);
    let n_on = frame_loads_in_asm(&on);
    assert!(
        n_off > n_on,
        "关掉 R_RAW 之后帧载入没有变多（关 {n_off} / 开 {n_on}）⇒ S1 补全的写集其实没有被 \
         任何热路径用到，本条判据会永远绿；这时应当删掉这一位而不是留着它"
    );
    assert!(
        insn_count(&on) < insn_count(&off),
        "R_RAW 不减少总指令数（关 {} / 开 {}）",
        insn_count(&off),
        insn_count(&on)
    );
    // 反向不变量：开着 R_RAW 绝不能**增加**任何一次访存（放宽只会少发，不会多发）。
    assert!(n_on <= n_off);
}

/// 判据 2：A1 打开后四档结果必须仍然一致，且不得把任何一次必须做的重载削掉。
///
/// 这一条主张的是**安全边界**而不是收益：Gate 0a 在 `luaV_execute` 上量到
/// `hit_call = 0`，也就是「调用挡住的转发」在这个负载上根本不存在。因此这里不写
/// 「A1 应当减少载入」这种主张 —— 那会把一个还没证实的收益写成判据。A1 的单位级
/// 两臂差异由 `blockopt::tests::a1_call_split_is_gated_by_its_own_bit` 与
/// `a1_still_kills_caller_saved_facts` 钉住。
#[test]
fn slot_fact_survives_call_equivalence() {
    // call 密集 + 局部被反复存取：A1 若把「callee 能写到的槽」当成可留，立刻错值。
    const SRC: &str = r#"
struct Box { int a, b; };
static int sink(struct Box *p) { p->a = p->a * 3 + 1; return p->b; }
static int run(int n) {
    struct Box q;
    q.a = n;
    q.b = n + 7;
    int s = 0;
    for (int i = 0; i < 6; i++) {
        s = s + sink(&q);          /* 每次 call 都可能改写 q 的那个槽 */
        q.b = q.a + i;             /* 存槽 */
        s = s + q.b;               /* 取同槽：A1 若在此处误转发就是错值 */
    }
    return s + q.a;
}
int main(void) {
    int r = run(5);
    return r > 0 ? 0 : 1;
}
"#;
    // 取 gcc 的同一份结果作为外部对照（不以 dcc 自证）。
    let gcc = run_gcc_cross_check("a1-equiv", SRC);
    for (lvl, tag) in [("-O0", "O0"), ("-O1", "O1"), ("-O2", "O2"), ("-O3", "O3")] {
        for (rules, name) in [(A1_ON, "a1"), (DEF_NOW, "base")] {
            let (o, e) = run_compile_and_run_env(&format!("a1-{tag}-{name}"), SRC, &[lvl], &[("DCC_BLOCKOPT_RULES", rules)]);
            assert!(o.status.success(), "{lvl}/{name} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
            assert_eq!(run_exe(&e), gcc, "{lvl} 开 {name} 档算出与 gcc 不同的结果");
        }
    }
    // 门控不变量：关 A1 必须与今天逐字节相同（否则 5454 组合的门关档就漂了）。
    let base = compile_asm_text_with_env("a1-base", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", DEF_NOW)]).0;
    let a1 = compile_asm_text_with_env("a1-on", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", A1_ON)]).0;
    let (b, a) = (frame_loads_in_asm(&base), frame_loads_in_asm(&a1));
    assert!(
        a <= b,
        "开 A1 帧载入反而变多了（基线 {b} / A1 {a}）⇒ 分流把某个必须重载的槽当成了可转发"
    );
}

/// 判据 3：只能从落空进入的标签可以继承事实（A2 的那一臂）。
#[test]
fn slot_fact_crosses_join_label() {
    const SRC: &str = r#"
static int acc;
static int bump(int v) { acc = acc + v; return acc; }
int run(int n) {
    int keep = n + 100;
    if (n > 1000) { return bump(keep); }   /* 这条路直接 return，不落到下面 */
    int t = bump(keep);                    /* 从上面落空进来 */
    return keep + t;
}
int main(void) { return run(3) == 106 ? 0 : 1; }
"#;
    let base = compile_asm_text_with_env("a2-base", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", DEF_NOW)]).0;
    let a2 = compile_asm_text_with_env("a2-on", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", A2_ON)]).0;
    let gcc = run_gcc_cross_check("a2-equiv", SRC);
    assert_eq!(run_exe_and_check("a2-run", SRC, A2_ON), gcc, "A2 开着就算错值");
    let (b, a) = (frame_loads_in_asm(&base), frame_loads_in_asm(&a2));
    assert!(
        a <= b,
        "A2 让帧载入变多了（基线 {b} / A2 {a}）⇒ 继承判据把某个跳转目标也算成了落空"
    );
}

/// 判据 3b：多前驱取交那一臂（方案 §8.3-2 要求的「发射后 pass」的另一半天）。
///
/// 形状：两条**前向** `goto` 落进同一个用户标签。那个标签的前驱集在定义它之前已经
/// 全部数到 ⇒ 可以按取交保留，而不是整体清。反面（循环头/边还没发完）由判据 4 钉。
///
/// 红法（两条各自能红）：
///   1. 把 `intersect_with` 改成不交（直接拿当前状态）⇒ `a2_intersect` 读数仍在但值会错；
///   2. 把 `jmp_rel32` 里的 `a2_contribute` 拆掉 ⇒ `a2_intersect` 读数归零 ⇒ 本条变红
///      （那等于「取交那一臂从未跑过」，而方案不允许拿一个没跑的臂当作已实现）。
#[test]
fn slot_fact_intersects_at_multi_pred_join_label() {
    const SRC: &str = r#"
/* `sink` 必须在本 TU 里有定义：gcc 交叉对拍那个助手是单 TU 链接的，
   只给原型会在 ld 阶段报 undefined reference（实测本轮就是在这里红的）。
   `noinline` 是为了保住「一次 call 之后进两条 goto 边」这个形状。*/
static __attribute__((noinline)) long sink(long v) { return v + 1; }
int join(int c, long s) {
    long a = sink(s);
    if (c == 1) { goto at; }
    if (c == 2) { goto at; }
    return -1;
at:
    return (int)(a + c);
}
int main(void) { return join(1, 3) + join(2, 5) + join(7, 9) >= -3 ? 0 : 1; }
"#;
    // 三条读数都得在：计数开着才有取交，关着必须一处也没有。
    let cnt = |rules: &str| -> u64 {
        let (_o, e) = compile_asm_text_with_env(
            "a2j",
            SRC,
            &["-O2"],
            &[("DCC_BLOCKOPT_RULES", rules), ("DCC_KILL_ALL_STAT", "1")],
        );
        e.lines()
            .filter(|l| l.contains("site=a2_intersect"))
            .filter_map(|l| l.split("hits=").nth(1))
            .filter_map(|t| t.split(|c: char| !c.is_ascii_digit()).next())
            .map(|v| v.parse::<u64>().unwrap_or(0))
            .sum()
    };
    let on = cnt(A2_ON);
    let off = cnt(DEF_NOW);
    assert!(on > 0, "开 R_LABELJOIN 却一次取交也没发生 ⇒ 前驱边没被记下来（取交那一臂未接）");
    assert_eq!(off, 0, "关着 R_LABELJOIN 也能取交 ⇒ 那道位没作用到交汇点（门控假了）");
    // 值判据：与 gcc 交叉对拍，两臂都要对。
    let gcc = run_gcc_cross_check("a2j-equiv", SRC);
    for (rules, tag) in [(DEF_NOW, "off"), (A2_ON, "on")] {
        let (o, e) = run_compile_and_run_env(&format!("a2j-{tag}"), SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", rules)]);
        assert!(o.status.success(), "{tag} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(run_exe(&e), gcc, "{tag} 档与 gcc 结果不同");
    }
}

/// 判据 4：循环头必须照旧重载 —— 这是判据 3 的反面护栏。
#[test]
fn slot_fact_not_kept_at_loop_head() {
    const SRC: &str = r#"
static int sink(int v) { return v ^ 0x5A5A; }
int run(int n) {
    int keep = n + 50;
    for (int i = 0; i < 4; i++) {
        keep = keep + sink(i);   /* 循环头是跳转目标：事实必须已作废 */
    }
    return keep;
}
int main(void) { return run(3) != 0 ? 0 : 1; }
"#;
    let base = compile_asm_text_with_env("lh-base", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", DEF_NOW)]).0;
    let a2 = compile_asm_text_with_env("lh-a2", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", A2_ON)]).0;
    let body = |t: &str| {
        t.split_inclusive("\nrun:")
            .nth(1)
            .map(|s| frame_loads_in_asm(s))
            .unwrap_or_else(|| panic!("夹具里没有 run 的函数体"))
    };
    assert_eq!(
        body(&base),
        body(&a2),
        "A2 改变了循环头的帧载入次数（基线 {} / A2 {}）：循环头必须照旧整体清事实",
        body(&base),
        body(&a2)
    );
    let gcc = run_gcc_cross_check("lh-equiv", SRC);
    assert_eq!(run_exe_and_check("lh-run", SRC, A2_ON), gcc, "循环头那一臂算错值");
}

/// 判据 5：`__builtin_setjmp` 的接收帧不得绑 caller-saved 家（方案 §4.1）。
///
/// 快照只存 callee-saved 那六个号（rbx/rbp/r12–r15）与 rsp/r13，而 `__builtin_longjmp`
/// 只恢复 rsp/r13 再跳 —— 落在 r8/r9 上的家没有任何地方恢复它。
///
/// `acc` 在 setjmp 与 longjmp 之间**不被改写**：这样「回到接收点时 acc 还是原值」是
/// C 保证的，而跨 longjmp 的 caller-saved 家一定读到垃圾（中间那次 call 就破坏它）。
const LONGJMP_SRC: &str = r#"
static int buf[16];
static int thrown;
static void raiser(void) {
    thrown = 42;
    __builtin_longjmp(buf, 1);
}
int main(void) {
    int acc = 7;                       /* 正是会被绑成家的热变量 */
    if (__builtin_setjmp(buf) == 0) {
        raiser();
        return 1;                      /* 不该走到这里 */
    }
    if (thrown != 42) return 2;
    return acc == 7 ? 0 : 3;           /* 3 = 拿到的是没被恢复过的号 */
}
"#;

/// 判据 5：`__builtin_setjmp` 必须被当成二次返回点（方案 §4.1）。
///
/// 为什么拿「影响域认得」而不是「跳回来值对」做判据：实测叓现一个与本方案无关的
/// **既有缺陷**——dcc 的 `__builtin_longjmp` 会让 longjmp 之前对全局的写入丢掉
/// （`thrown = 42; __builtin_longjmp(...)` 之后 `thrown` 读回 0，gcc 是 42；该缺陷在
/// `DCC_BLOCKOPT=0` 的 -O0 下同样存在，不属本轮任何一条改动）。所以那一部分不能写成
/// 断言（写了就是把一个已知不成立的期望伪造成“已验证”），已单独立项记进报告。本条只钉
/// §4.1 真正修的那件事：二次返回点被认出来 ⇒ 接收帧不再有任何寄存器家（包含 r8/r9）。
#[test]
fn nonlocal_home_longjmp_ext() {
    // 正面判据：含 `__builtin_setjmp` 的函数必须落进 returns_twice 影响域（被拉出 SSA）。
    // 红法：从 `RETURNS_TWICE_NAMES` 删掉这两个名字 ⇒ 下面那句 assert 立即变红。
    let (_, err) = compile_asm_text_with_env("lj-bail", LONGJMP_SRC, &["-O2"], &[("DCC_SSA_BAIL", "1")]);
    assert!(
        err.contains("SSA bail: returns_twice cone (main)"),
        "__builtin_setjmp 没被认成二次返回点，因此它的接收帧仍可绑 caller-saved 家，\n\
         而快照只存 callee-saved 那六个号 ⇒ 跨 longjmp 就是错值（方案 §4.1）。stderr=\n{err}"
    );
    // 反面判据（同一个形状换成普通函数）：影响域不是「把所有函数都拉出去」。
    const PLAIN: &str = r#"
static int mark(int v) { return v + 1; }
int main(void) { int acc = 7; acc = acc + mark(acc); return acc == 15 ? 0 : 1; }
"#;
    let (_, err_plain) = compile_asm_text_with_env("plain-bail", PLAIN, &["-O2"], &[("DCC_SSA_BAIL", "1")]);
    assert!(
        !err_plain.contains("returns_twice cone (main)"),
        "不含二次返回点的普通函数也被拉出了影响域 ⇒ 认得判据发成了「全部拒绝」"
    );
    // 四档不得崩（值结果不在此主张，理由见上面那段）。
    for lvl in ["-O0", "-O1", "-O2", "-O3"] {
        let (o, e) = run_compile_and_run_env(&format!("lj-{lvl}"), LONGJMP_SRC, &[lvl], &[]);
        assert!(o.status.success(), "{lvl} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
        let rc = run_exe(&e);
        assert!(rc < 128, "{lvl} 下夹具被信号打死（rc={rc}）");
    }
}

/// 区内家（S4）默认关时必须逐字节不变；开着时必须真的在发射管线上动过手。
///
/// 为什么两条要写在同一条用例里：只验「关时不变」，可以靠一个从未被接进管线的开关拿到；
/// 只验「开着会动手」，又分不清是机制生效还是默认档本来就会漂。两个一起才是
/// 「这个旋钮接得上、且默认没偷开」。
///
/// 为什么判据不拿「产物字节不同」而是拿读数：实测在 Lua 语料上本机制的服务为零
/// （`loads_saved=0`），因为发射约定把求值结果恒落 `%rax`，那个号下一条就被踩掉 ⇒
/// 家活不到下一次访问。拿「产物必须变」当判据会把一个**诚实读数为零**的机制钉成
/// 假绿（要么它被改成会漂，要么本条被删）。现在的三条读数一起才是真相：
///   1. 见证点跑过（`regionstat` 行存在且计数非零）；
///   2. 那道 callee-saved 的门确实在拦（`refuse(caller_saved=…)` > 0）；
///   3. 门关掉后确实能建家（`promoted>` 0）—— 也就是「机制未命中」而不是「机制未接」。
#[test]
fn region_home_is_gated_off_and_live_when_on() {
    const SRC: &str = r#"
struct P { long a, b, c; };
/* `sink` 必须在同一个 TU 里有定义：gcc 交叉对拍那个助手是单 TU 链接的，
   而「已知原型的非变参函数」恰好就是本用例要看的那一种形状。*/
static long sink(long v) { return v + 1; }
long run(long n) {
    struct P p;
    p.a = n + 1; p.b = n + 2; p.c = n + 3;
    long t = sink(p.a) + sink(p.b) + sink(p.c);
    return p.a + p.b + p.c + t;
}
int main(void) { return run(5) > 0 ? 0 : 1; }
"#;
    let off = compile_asm_text_with_env("rg-off", SRC, &["-O2"], &[("DCC_REGION_HOME", "0")]).0;
    let def = compile_asm_text_with_env("rg-def", SRC, &["-O2"], &[]).0;
    // 比之前先剔掉 `.file "…"` 那一行：助手把夹具写到带名字的临时路径里，不剔的话
    // 「两臂同文件」会被文件名差异抖成假红（实测就是它）。
    let strip = |s: &str| -> String {
        s.lines()
            .filter(|l| !l.trim_start().starts_with(".file"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (off, def) = (strip(&off), strip(&def));
    assert_eq!(off, def, "默认档与显式关的产物不同 ⇒ S4 的旋钮偷开了");
    // 两臂都得算对（开着时无论如何不得错值）。
    let gcc = run_gcc_cross_check("rg-equiv", SRC);
    for (tag, env) in [
        ("off", &[("DCC_REGION_HOME", "0")][..]),
        ("on", &[("DCC_REGION_HOME", "1")][..]),
        ("any", &[("DCC_REGION_HOME", "1"), ("DCC_REGION_CALLEE", "0")][..]),
    ] {
        let (o, e) = run_compile_and_run_env(&format!("rg-{tag}"), SRC, &["-O2"], env);
        assert!(o.status.success(), "{tag} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(run_exe(&e), gcc, "{tag} 档与 gcc 结果不同");
    }
    // 「接得上」的证据：逐函数读数必须报出本层真的看过这些指令。
    let stat_of = |env: &[(&str, &str)]| -> String {
        let (o, e) = compile_asm_text_with_env("rg-stat", SRC, &["-O2"], env);
        assert!(o.lines().count() > 0, "产物为空");
        e
    };
    let gated = stat_of(&[("DCC_REGION_HOME", "1"), ("DCC_REGION_STAT", "1")]);
    let ungated = stat_of(&[
        ("DCC_REGION_HOME", "1"),
        ("DCC_REGION_STAT", "1"),
        ("DCC_REGION_CALLEE", "0"),
    ]);
    let num = |s: &str, key: &str| -> u64 {
        s.lines()
            .filter(|l| l.contains("regionstat"))
            .filter_map(|l| l.split(key).nth(1))
            .filter_map(|t| t.trim_start_matches('=').split(|c: char| !c.is_ascii_digit()).next())
            .map(|v| v.parse::<u64>().unwrap_or(0))
            .sum()
    };
    assert!(
        gated.contains("regionstat"),
        "旋钮开着却一行读数也没有 ⇒ 见证点根本没接进 emit_insn（S4 是个假开关）"
    );
    assert!(
        num(&gated, "caller_saved") > 0,
        "callee-saved 那道门一次也没拦 ⇒ 要么门写错了，要么本层根本没看到存出/载入。读数：{gated}"
    );
    assert!(
        num(&ungated, "promoted") > 0,
        "把门关掉仍然一个家也建不起来 ⇒ 提升点匹配不上任何真实指令（机制未接）"
    );
}

/// 编译到可重定位对象（多个 flag + env 旋钮），拿字节回来交给下面两个 readelf 助手。
///
/// 为什么不走 `run_compile_object_env`：它只接一个优化参数，而本用例同时需要 `-O2` 与
/// `-g` 两个（loclist 只在 -O2 的家分配之后才有意义）。
fn region_obj_with_env(
    name: &str,
    source: &str,
    flags: &[&str],
    envs: &[(&str, &str)],
) -> Vec<u8> {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-{name}-{}.o", std::process::id()));
    std::fs::write(&src, source).expect("write fixture");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.args(["-std=gnu23"]).args(flags).args(["-c"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.args(["-o"]).arg(&obj).arg(&src).output().expect("run dcc");
    assert!(out.status.success(), "编译失败: {}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&obj).unwrap_or_default();
    let _ = std::fs::remove_file(src);
    let _ = std::fs::remove_file(obj);
    bytes
}

/// 一个对象的节名清单（写成一行，给 `contains(".debug_loc")` 这种存在性判据用）。
///
/// `readelf -SW` 的每一行形如 `  [13] .debug_loc PROGBITS ...` ⇒ 节名是第二个字段；
/// 表头/空行拿不到以 `.` 开头的字段，自然被滤掉。
fn elf_section_names(obj: &[u8]) -> String {
    let text = elf_readelf(obj, &["-SW"]);
    text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let _ = it.next()?;
            it.next()
        })
        .filter(|n| n.starts_with('.'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 把某个节 dump 成人读文本（用于「能不能解」这一类结构判据）。
fn elf_dump_section(obj: &[u8], sec: &str) -> String {
    let name = sec.trim_start_matches('.');
    elf_readelf(obj, &["--wide", "--debug-dump", &format!("{name}")])
}

fn elf_readelf(obj: &[u8], args: &[&str]) -> String {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("dcc-readelf-{}-{}.o", std::process::id(), args.join("-").replace(['/', '.'], "_")));
    std::fs::write(&path, obj).expect("write obj for readelf");
    let out = std::process::Command::new("readelf").args(args).arg(&path).output();
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.expect("run readelf").stdout).into_owned()
}

/// 编译 + 运行 + 取退出码（带 `DCC_BLOCKOPT_RULES` 下发的那一档）。
fn run_exe_and_check(name: &str, source: &str, rules: &str) -> i32 {
    let (o, e) = run_compile_and_run_env(name, source, &["-O2"], &[("DCC_BLOCKOPT_RULES", rules)]);
    assert!(o.status.success(), "{name} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
    run_exe(&e)
}

/// A3 的门控不变量：溢出淘汰只影响「事实表还剩多少」，不影响任何值。
///
/// 这一条不主张收益：Gate 0a 在 `luaV_execute` 上量到 `hit_capacity = 0`，也就是
/// 16 槽容量门在那个数上根本不绑。单位级的两臂差异由
/// `blockopt::tests::slot_capacity_evicts_one_not_all` 钉住。
#[test]
fn slot_capacity_lru_is_value_neutral() {
    const SRC: &str = r#"
static int sink(int v) { return v * 3 + 1; }
int wide(int n) {
    int a0 = n, a1 = n + 1, a2 = n + 2, a3 = n + 3, a4 = n + 4, a5 = n + 5;
    int a6 = n + 6, a7 = n + 7, a8 = n + 8, a9 = n + 9, a10 = n + 10, a11 = n + 11;
    int a12 = n + 12, a13 = n + 13, a14 = n + 14, a15 = n + 15, a16 = n + 16;
    int a17 = n + 17, a18 = n + 18, a19 = n + 19;
    int s = 0;
    /* 20 个活跃局部：帧槽数超过 MAX_SLOTS=16，正是容量臂的形状 */
    s += sink(a0) + sink(a1) + sink(a2) + sink(a3) + sink(a4) + sink(a5);
    s += sink(a6) + sink(a7) + sink(a8) + sink(a9) + sink(a10) + sink(a11);
    s += sink(a12) + sink(a13) + sink(a14) + sink(a15) + sink(a16) + sink(a17);
    return s + a18 + a19;
}
int main(void) { return wide(2) > 0 ? 0 : 1; }
"#;
    let gcc = run_gcc_cross_check("a3-equiv", SRC);
    for (tag, rules) in [("off", DEF_NOW), ("on", A3_ON)] {
        let r = run_exe_and_check(&format!("a3-{tag}"), SRC, rules);
        assert_eq!(r, gcc, "{tag} 档与 gcc 结果不同");
    }
    let off = compile_asm_text_with_env("a3-off", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", DEF_NOW)]).0;
    let on = compile_asm_text_with_env("a3-on", SRC, &["-O2"], &[("DCC_BLOCKOPT_RULES", A3_ON)]).0;
    let (a, b) = (frame_loads_in_asm(&off), frame_loads_in_asm(&on));
    assert!(
        b <= a,
        "开 A3 帧载入变多了（关 {a} / 开 {b}）：容量淘汰不能新增任何一次访存"
    );
}

/// 方案 §4.4：`-g` 下的位置必须能分段（loclist），而区内家不得依赖它。
///
/// 两件分开的事，本条同时钉住：
///   1. **区内家（S4）带 `-g` 可以开工**：本层从不推迟存出（内存一直是权威），单
///      `DW_AT_location` 就是正确答案。红法：拿硬互斥把 `-g` 那一档关掉 ⇒ 下面的
///      `allowed=true` 断言变红（那等于本层还在逃 §4.4，而不是满上它）；
///   2. **今天确实有分段位置要描述**：延伸池（caller-saved）家在 call 序列里会被存进
///      保存槽，那段区间里号里的值会被 callee 破坏。不开 loclist 时 `-g` 产物里根本
///      不会有 `.debug_loc`，而调试器在调用期间会读一个错号。
#[test]
fn debug_info_loclists_describe_home_windows_and_region_needs_none() {
    // POOL 满 + 多个跨 call 的活变量 ⇒ 至少一个家落在延伸池上。
    const SRC: &str = r#"
static __attribute__((noinline)) long sink(long v) { return v + 1; }
long run(long n) {
    long k1 = n + 1, k2 = n + 2, k3 = n + 3, k4 = n + 4, k5 = n + 5, k6 = n + 6;
    long t = 0;
    for (int i = 0; i < 4; i++) {
        t += sink(k1) + sink(k2) + sink(k3) + sink(k4) + sink(k5) + sink(k6);
        k1 += i; k2 += i; k3 += i; k4 += i; k5 += i; k6 += i;
    }
    return t + sink(k1) + sink(k2) + sink(k3) + sink(k4) + sink(k5) + sink(k6);
}
int main(void) { return run(5) > 0 ? 0 : 1; }
"#;
    // 1) 带 -g 时区内家开工，而它不得产生任何区间（本层不推迟存出）。
    let (asm_on, err_on) = compile_asm_text_with_env(
        "g-rg",
        SRC,
        &["-O2", "-g"],
        &[("DCC_REGION_HOME", "1"), ("DCC_REGION_STAT", "1")],
    );
    assert!(!asm_on.is_empty(), "带 -g 编译失败: {err_on}");
    assert!(
        err_on.contains("allowed=true"),
        "带 -g 时区内家不开工 ⇒ 本层还靠硬互斥逃面，没有真去满足 §4.4"
    );
    // 2) 带 -g 且确实有延伸池存/取窗口时，产物里必须有解得开的 `.debug_loc`。
    //    拿仓库里那份真的会拿到延伸池家的夹具（POOL 满 + 多个跨 call 的活变量才能把家
    //    挤到 caller-saved 的延伸池号上；自己编的小夹具实测拿不到，那这个断言就只能靠
    //    真语料），而不是拿一个永远不会成立的形状当判据。
    let src_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/c/arrow_on_array_member.c");
    let corpus = std::fs::read_to_string(src_path).expect("读夹具 arrow_on_array_member.c");
    let obj = region_obj_with_env("g-loc", &corpus, &["-O2", "-g"], &[]);
    let sections = elf_section_names(&obj);
    assert!(
        sections.contains(".debug_loc"),
        "-g 产物里没有 `.debug_loc`，而这份夹具里有住在延伸池家里的变量 ⇒ 调用期间调试器会读\n\
         一个被 callee 破坏的号（§4.4 要 loclist 的那个理由）。节表：{sections}"
    );
    // 区间必须单调且能解（写错地址口径时 readelf 报 unused bytes 或 begin>end）。
    let dump = elf_dump_section(&obj, ".debug_loc");
    assert!(
        !dump.contains("unused bytes") && !dump.contains("开始 > 结束"),
        "`.debug_loc` 解不干净：\n{dump}"
    );
    assert!(
        dump.contains("DW_OP_fbreg") && dump.contains("DW_OP_reg"),
        "loclist 里必须同时看到「号里」与「保存槽里」两种位置（只有一种就等于没分段）：\n{dump}"
    );
    // 反向对照：不开 `-g` 时不得多出这个节（新机制不得动旧字节）。
    let obj_nog = region_obj_with_env("g-nog", &corpus, &["-O2"], &[]);
    assert!(
        !elf_section_names(&obj_nog).contains(".debug_loc"),
        "不开 -g 的产物里多了 `.debug_loc` ⇒ 新机制改了旧字节"
    );
    // 值判据：开与关、带不带 -g 都算得与 gcc 一样。
    let gcc = run_gcc_cross_check("g-loc-equ", SRC);
    for (tag, flags, knob) in
        [("g0", &["-O2", "-g"][..], "1"), ("g1", &["-O2", "-g"][..], "0"), ("g2", &["-O2"][..], "1")]
    {
        let (o, e) =
            run_compile_and_run_env(&format!("g-loc-{tag}"), SRC, flags, &[("DCC_REGION_HOME", knob)]);
        assert!(o.status.success(), "{tag} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(run_exe(&e), gcc, "{tag} 档与 gcc 结果不同");
    }
}

/// 判据 5b（方案 §5 点名的 `region_home_volatile_memops`）：volatile 局部必须零绑定，
/// 而且它那六次访问一次都不能被合提。
///
/// 为什么与单位那条 `volatile_slot_is_neither_forwarded_nor_deduplicated` 分开：那条只验
/// 事实层不建 volatile 槽的等价；本条验**产物层**两个后果：
///   * 区内家（旋钮开着）不能把 volatile 槽绑到一个号上（`refuse(vol=…)` 必须非零，
///     否则就是门未接）；
///   * 调试信息不能把它说成寄存器位置（`DW_OP_fbreg`，不是 `DW_OP_reg*`）——一个 volatile
///     对象的每次访问都是可观察事件，把它说成住在号里就是允许调试器错过那些写。
/// 再加上一条硬数量判据：函数体里带 `(%r13)` 的行必须把六次访问全跳出来（转发/合并
/// 任何一次都是错），且四档与 gcc 同结果。
#[test]
fn region_home_volatile_memops_binds_nothing_and_keeps_six_accesses() {
    const SRC: &str = r#"
int readn(void);
long run(long n) {
    volatile long v = n;      /* 1 写 */
    v = v + 1;                /* 2 读 + 3 写 */
    v = v + readn();          /* 4 读 + 5 写 */
    return v;                 /* 6 读 */
}
int readn(void) { return 2; }
int main(void) { return run(3) == 8 ? 0 : 1; }
"#;
    let (asm_on, err_on) = compile_asm_text_with_env(
        "vh-on",
        SRC,
        &["-O2", "-g"],
        &[("DCC_REGION_HOME", "1"), ("DCC_REGION_STAT", "1")],
    );
    let body = extract_function_body(&asm_on, "run");
    // 六次对 `v` 的访问：本函数只这一个局部，所以带 (%r13) 的访存行就是它（callee-saved
    // 的序言/尾声保存行不叫 `(%r13)` 而叫 -0x10(%r13) 那些，它们同样计入，所以判据取
    // 「不少于六次」而不是「恰好六次」：少了就是被转发/合提，那才是本条要红的事。
    let memops = body
        .lines()
        .filter(|l| l.contains("(%r13)") && !l.trim_start().starts_with("lea"))
        .count();
    assert!(
        memops >= 6,
        "volatile 槽的六次访问被提掉了（只剩 {memops} 次访 %r13 的行）：volatile 的每一次访问\n\
         都是可观察事件，转发/合并它就是改变语义。函数体：\n{body}"
    );
    // 区内家必须真的不碰这个函数：护栏里 volatile 是**函数级**判定（§4.2），所以这里要
    // 看到的是「护栏=false ⇒ allowed=false」，而不是“进了见证层再拒”。两种都叫零绑定，
    // 但只有前者才是护栏真正接上了线（后者说明 `volatile_obj` 没被算出来）。
    assert!(
        err_on.contains("fn=run 护栏=false") && err_on.contains("allowed=false"),
        "带 volatile 局部的函数仍然进了区内家 ⇒ §4.2 的函数级护栏没接上（零绑定不成立）。\n\
         读数：{err_on}"
    );
    // 调试信息：volatile 局部只能被说成栈槽位置，不能是寄存器家。
    let obj = region_obj_with_env("vh-obj", SRC, &["-O2", "-g"], &[("DCC_REGION_HOME", "1")]);
    // 拿行序找：读到了 `DW_AT_name ... : v` 之后，往后最多四行里必须出现它的
    // `DW_AT_location`。
    let info = elf_dump_section(&obj, ".debug_info");
    let lines: Vec<&str> = info.lines().collect();
    let mut loc_line = String::new();
    for (i, l) in lines.iter().enumerate() {
        if l.contains("DW_AT_name") && l.trim_end().ends_with(": v") {
            loc_line = lines[i..(i + 4).min(lines.len())]
                .iter()
                .find(|n| n.contains("DW_AT_location"))
                .unwrap_or(&"<本 DIE 没有 DW_AT_location>")
                .to_string();
            break;
        }
    }
    assert!(
        !loc_line.is_empty(),
        "在 `.debug_info` 里找不到局部 `v` 的 DIE（那等于 -g 下这个变量根本来就不存在）。\n\
         dump 开头：\n{}",
        lines.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
    assert!(
        loc_line.contains("DW_OP_fbreg") && !loc_line.contains("DW_OP_reg"),
        "volatile 局部 `v` 的位置被说成了寄存器家（零绑定不成立）：\n{loc_line}"
    );
    // 值判据：四档 × 旋钮开/关都要与 gcc 同结果。
    let gcc = run_gcc_cross_check("vh-equ", SRC);
    for lvl in ["-O0", "-O1", "-O2", "-O3"] {
        for knob in ["0", "1"] {
            let (o, e) = run_compile_and_run_env(
                &format!("vh-{lvl}-{knob}"),
                SRC,
                &[lvl],
                &[("DCC_REGION_HOME", knob)],
            );
            assert!(o.status.success(), "{lvl}/{knob} 编译失败");
            assert_eq!(run_exe(&e), gcc, "{lvl} 旋钮={knob} 与 gcc 结果不同");
        }
    }
}

/// S6 的合并机制：两条全等的冷尾必须合成一条，且不改变任何值。
///
/// 判据为什么写成「不加重 + 值等价」而不是「必须合掉 N 对」：实测 Lua 上合了 0 对。
/// 原因是 80 条冷尾各自的汇合点不同（分派循环的头部被复制进每一条臂），合并判据
/// 「字节 + 块内跳转目标全等」因此永不成立。这一条因此只守机制本身正确，而把
/// 「到底合了几对」交给 `flush_cold_arms` 的 `coldmerge` 读数（gate0 的脚本采）。
#[test]
fn cold_tail_merge_is_value_neutral() {
    const SRC: &str = r#"
static int log1(int n) { return n + 1; }
static int log2(int n) { return n + 2; }
int run(int n) {
    int r = n;
    if (__builtin_expect(n > 1000000, 0)) { r = log1(r) + 7; }
    if (__builtin_expect(n < -1000000, 0)) { r = log2(r) + 9; }
    /* 两条形状相同、汇合点也相同的冷尾：合并判据必须能对上 */
    if (__builtin_expect(n == 424242, 0)) { r = log1(r) + 7; }
    return r;
}
int main(void) { return run(5) == 5 ? 0 : 1; }
"#;
    let off = &[("DCC_COLD_OUTLINE", "1"), ("DCC_COLD_MERGE", "0")][..];
    let on = &[("DCC_COLD_OUTLINE", "1"), ("DCC_COLD_MERGE", "1")][..];
    let gcc = run_gcc_cross_check("merge-equiv", SRC);
    for (tag, env) in [("off", off), ("on", on)] {
        let (o, e) = run_compile_and_run_env(&format!("merge-{tag}"), SRC, &["-O2"], env);
        assert!(o.status.success(), "{tag} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(run_exe(&e), gcc, "{tag} 档与 gcc 结果不同");
    }
    // 合并的字节判据：把重复体删掉必须让对象变小或不变，绝不允许变大。
    let (_, b_off) = run_compile_object_env("merge-bytes-off", SRC, "-O2", off);
    let (_, b_on) = run_compile_object_env("merge-bytes-on", SRC, "-O2", on);
    assert!(
        b_on.len() <= b_off.len(),
        "开合并后对象反而变大了（{} → {}）：回退重复体时没有把字节截干净",
        b_off.len(),
        b_on.len()
    );
}

// ===== 调用点栈对齐（SysV AMD64：`call` 指令处 rsp ≡ 0 mod 16）=====
//
// 红法：删掉 `emit_insn` 里 `stack_shift` 对 `sub/add rsp, imm` 的记账
// （instructions.rs 的 `rsp_imm_delta`）⇒ 外层调用已下推出参区之后，内层调用
// 仍按陈旧的偏移再补一次 8 ⇒ 本条在第一个 `call` 处读到 rsp ≡ 8 ⇒ 变红。
//
// 语料取自 SQLite amalgamation 第 47589 行
// `pPath->rc = unixLogError(SQLITE_CANTOPEN_BKPT, "lstat", zIn);` 的形状：
// 左值地址要跨调用活着（push），外层调用的实参里还有一次内层调用。
// 这正是 testfixture 在 oserror-1.4.1 崩在 libtcl 的 `movaps` 上的那个形状。
const NESTED_CALL_ALIGN_SRC: &str = r#"
struct P { int rc; };
int lasterr(int e);
int logerr(int code, const char *fn, const char *path, int line);
int f(struct P *p, const char *path, int line) {
    p->rc = logerr(lasterr(line), "lstat", path, line);
    return p->rc;
}
int main(void) { return 0; }
"#;

/// 按 `-S` 文本逐行累加 rsp 偏移，返回每个 `call` 处的偏移。
///
/// 起点取 8：ABI 规定 `call` 刚执行的函数入口 rsp ≡ 8 (mod 16)。只认本后端
/// 对「改变 rsp」的四种写法（`push`/`pop`/`sub $N, %rsp`/`add $N, %rsp`）。
/// 遇到 `mov %r13, %rsp` 这类把 rsp 拉回帧基的复位就不再累加 —— 判据只对
/// 累加段成立，含糊的地方不猜（实测 f 的形状里没有它）。
fn call_site_rsp_offsets(asm: &str) -> Vec<i64> {
    let mut off = 8i64;
    let mut calls = Vec::new();
    for line in asm.lines() {
        let l = line.trim();
        if l == "mov %r13, %rsp" {
            break;
        }
        if let Some(n) = l
            .strip_prefix("sub $")
            .and_then(|r| r.strip_suffix(", %rsp"))
        {
            off -= n.trim().parse::<i64>().expect("sub rsp 立即数");
        } else if let Some(n) = l
            .strip_prefix("add $")
            .and_then(|r| r.strip_suffix(", %rsp"))
        {
            off += n.trim().parse::<i64>().expect("add rsp 立即数");
        } else if l.starts_with("push ") {
            off -= 8;
        } else if l.starts_with("pop ") {
            off += 8;
        } else if l.starts_with("call ") {
            calls.push(off);
        }
    }
    calls
}

#[test]
fn nested_call_sites_are_16_byte_aligned() {
    let (out, asm) =
        run_asm_output_test_flags("nested-call-align", NESTED_CALL_ALIGN_SRC, "-O1");
    assert!(
        out.status.success(),
        "dcc -O1 -S 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = extract_function_body(&asm, "f");
    assert!(!body.is_empty(), "汇编里找不到 f:\n{asm}");
    let offs = call_site_rsp_offsets(&body);
    assert_eq!(
        offs.len(),
        2,
        "预期 f 里两次调用（内层 lasterr + 外层 logerr），实际 {} 次:\n{body}",
        offs.len()
    );
    let bad: Vec<i64> = offs.iter().copied().filter(|o| o.rem_euclid(16) != 0).collect();
    assert!(
        bad.is_empty(),
        "call 处 rsp 未 16 对齐（偏移 {bad:?}；合法值只有 0）:\n{body}"
    );
}
