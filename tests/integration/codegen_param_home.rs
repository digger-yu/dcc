// included from tests/integration.rs — not a standalone cargo test target

// ===== 形参当家（`DCC_HOME_PARAMS`）的执行级与结构级判据 =====
//
// 为什么单独一族：`luaV_execute(lua_State *L, CallInfo *ci)` 每步要读的热值就是那两个
// 形参，而分配器候选表的真源是 `Function::original_locals`（完整集应是
// `original_locals ∪ params`，见 `ssa/build.rs` 的注释），只看前者时形参连候选都不是，
// 序言只能把入站寄存器存进栈槽、此后每一次读都是一次访存。实测这一项在解释器上是
// 6.8 次载入/步（`L` 3.82 + `ci` 3.00）。
//
// 判据要挡住的四类错：
//   1. **入站号相撞**。形参值来自 `rdi,rsi,rdx,rcx,r8,r9`。若允许把某个形参的家定在
//      r8/r9，参数个数 ≥5 时就会出现「前一个形参已搬进 r8、后面的形参再从 r8 读自己那份」。
//      `param_home_five_plus_args_values` 专打这一形状（结构判据在 regalloc 侧的
//      `param_home_only_callee_saved`，但那条只看表，这里要真跑一遍）。
//   2. **变参函数**。`va_start` 依赖入站寄存器区被原样保存下来；家在号上时这套机制还没
//      跟它对调 ⇒ 变参函数必须整体不动（字节相等是唯一的硬证）。
//   3. **volatile 形参**。读写次数本身就是语义。
//   4. **对照被环境污染**。整套用例可以带 `DCC_HOME_PARAMS=1` 跑（那时"关"臂必须显式
//      写 `=0`，否则子进程继承环境变量，两臂同形，判据空跑）。本文件的 off 臂一律显式
//      传 `DCC_HOME_PARAMS=0` 而不是"什么都不传"，就是这个原因。

/// 两档编译+运行，返回 `(rc_off, rc_on, out_off, out_on)`。
///
/// `off` 臂显式带 `DCC_HOME_PARAMS=0`：不显式写就等于把判据交给外部环境。
fn param_home_run(name: &str, src: &str) -> (i32, i32, String, String) {
    let off: &[(&str, &str)] = &[("DCC_HOME_PARAMS", "0")];
    let on: &[(&str, &str)] = &[("DCC_HOME_PARAMS", "1")];
    let (o1, e1) = run_compile_and_run_env(name, src, &["-O2"], off);
    let (o2, e2) = run_compile_and_run_env(name, src, &["-O2"], on);
    assert!(o1.status.success() && o2.status.success(), "编译失败");
    let go = |p: &std::path::PathBuf| {
        let out = Command::new(p).output().expect("run fixture");
        assert!(
            out.status.code().is_some(),
            "夹具被信号打死: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let a = go(&e1);
    let b = go(&e2);
    let _ = fs::remove_file(&e1);
    let _ = fs::remove_file(&e2);
    (a.0, b.0, a.1, b.1)
}

/// 两档编到对象，返回字节（结构判据用）。off 同样显式写 `=0`。
fn param_home_objects(name: &str, src: &str) -> (Vec<u8>, Vec<u8>) {
    let (_o1, b1) = run_compile_object_env(name, src, "-O2", &[("DCC_HOME_PARAMS", "0")]);
    let (_o2, b2) = run_compile_object_env(name, src, "-O2", &[("DCC_HOME_PARAMS", "1")]);
    (b1, b2)
}

/// 热形参真的落到号上：入站那笔 `mov 形参号 → 帧槽` 必须消失。
///
/// 只看结果值不够——不搬家也一样算得对；只有「入口不再把它写进栈槽」才证明家生效了。
#[test]
fn param_home_removes_the_entry_store() {
    // `n` 在循环里被反复读：它是唯一该拿到 callee-saved 家的形参。
    let src = r#"
        extern int sink(int, int);
        int hot(int n) {
            int acc = 0;
            for (int i = 0; i < 40; i++) { acc += n + i; }
            return sink(acc, n);
        }
    "#;
    let (a, b) = param_home_objects("cold-param-hot", src);
    assert_ne!(a, b, "旋钮开了产物却没变 ⇒ 形参当家这条通道没接上");
    // 「入口把 %edi 写进帧槽」这一笔（`mov QWORD PTR [rbp/r13-..],rdi` 的字节形态）
    // 在开了之后必须不再出现：直接数入站号被写进内存的次数。
    let stores = |bs: &[u8]| -> usize {
        let mut c = 0usize;
        let mut i = 0usize;
        // 48 89 45 xx = mov [rbp/r13+disp8], rax 一族；这里只找 48 89 3F/45/… 里带
        // REX.B(0x49) 且 reg 字段=7(rdi) 的形参入库：49 89 3D/7D、48 89 47/7F 等形状。
        while i + 3 < bs.len() {
            // REX.W(|B) 89 /r，modrm.reg=7( edi/rdi )，mod≠3（内存目的）
            let rex = bs[i];
            if (rex == 0x48 || rex == 0x49) && bs[i + 1] == 0x89 && (bs[i + 2] >> 3) & 7 == 7
                && bs[i + 2] >> 6 != 3
            {
                c += 1;
                i += 3;
                continue;
            }
            i += 1;
        }
        c
    };
    assert!(
        stores(&b) < stores(&a),
        "开旋钮后入站 %rdi 仍被写进帧槽：off={} on={} ⇒ 家没交付到序言",
        stores(&a),
        stores(&b)
    );
}

/// 参数个数 ≥5 的形状：后面的形参从前一个形参的家号里读自己那份 = 静默错值。
///
/// 这是 `apply_bindings` 里「形参只接 POOL 号」那条限制的执行侧对偶；单看 regalloc 的
/// 表判据看不出序言有没有真按那个规则发指令，所以必须跑。
#[test]
fn param_home_five_plus_args_values() {
    let src = r#"
        int f(int a, int b, int c, int d, int e, int g, int h) {
            return a * 1 + b * 2 + c * 3 + d * 4 + e * 5 + g * 6 + h * 7;
        }
        int main(void) {
            int s = 0;
            for (int i = 0; i < 3; i++) s += f(i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6);
            return s % 251;   // 每圈 28i+112 ⇒ 112+140+168=420，420 % 251 = 169
        }
    "#;
    let (r1, r2, o1, o2) = param_home_run("cold-param7", src);
    assert_eq!(r1, r2, "参数个数 ≥5 时两档结果不同 ⇒ 入站号与家号相撞");
    assert_eq!(o1, o2);
    // 绝对值不对本判据负责（手工算错了两次）。改用**独立档位**当 oracle：同一份源码
    // 在 -O0（不参与寄存器家：`plan_auto_homes` 与延伸池都要求 level>=1）下的结果
    // 必须与 -O2 两档一致，同时这个结果本身不能是 0（三个档位同为 0 时谁都能过）。
    let (_o3, e3) = run_compile_and_run_env("cold-param7-o0", src, &["-O0"], &[]);
    assert!(e3.exists(), "-O0 对照没链接起来");
    let out3 = Command::new(&e3).output().expect("run -O0 fixture");
    let _ = fs::remove_file(&e3);
    assert_eq!(
        out3.status.code(),
        Some(r1),
        "-O2 与 -O0 不一致：不能拿它当参数家改动的反例对照"
    );
    assert_ne!(r1, 0, "夹具退化成恒零，任何实现都能过");
}

/// 变参函数一律不动：`va_start` 依赖入站寄存器区被原样保存。
#[test]
fn param_home_skips_variadic_functions() {
    let src = r#"
        #include <stdarg.h>
        int sum(int n, ...) {
            va_list ap; va_start(ap, n);
            int s = 0;
            for (int i = 0; i < n; i++) s += va_arg(ap, int);
            va_end(ap);
            return s;
        }
        int main(void) { return sum(4, 1, 2, 3, 4) == 10 ? 0 : 1; }
    "#;
    let (a, b) = param_home_objects("cold-variadic", src);
    assert_eq!(a, b, "变参函数被形参当家改动了 ⇒ 排除项失效");
    let (r1, r2, _, _) = param_home_run("cold-variadic", src);
    assert_eq!(r1, r2);
    assert_eq!(r1, 0, "夹具本身跑不出预期");
}

/// `volatile` 形参不得当家（读写次数本身就是语义）。
#[test]
fn param_home_skips_volatile_params() {
    let src = r#"
        int acc;
        void f(volatile int n) {
            for (int i = 0; i < 4; i++) acc += n;   /* 每次都要真的访存 */
        }
        int main(void) { f(7); return acc == 28 ? 0 : 1; }
    "#;
    let (a, b) = param_home_objects("cold-volatile", src);
    assert_eq!(a, b, "volatile 形参被绑当家 ⇒ 可观察的访存次数被改掉");
}

/// 默认位必须是关：不带旋钮时与显式 `=0` 逐字节相同。
#[test]
fn param_home_is_off_by_default() {
    let src = "int f(int n){int s=0;for(int i=0;i<n;i++)s+=n;return s;}\n";
    let (o1, b1) = run_compile_object_env("cold-param-default", &src, "-O2", &[]);
    let (o2, b2) = run_compile_object_env("cold-param-default", &src, "-O2", &[("DCC_HOME_PARAMS", "0")]);
    assert!(o1.status.success() && o2.status.success());
    assert_eq!(b1, b2, "默认位与显式关不一致 ⇒ 旋钮在默认路径上漏生效");
}
