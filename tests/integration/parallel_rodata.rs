// included from tests/integration.rs — not a standalone cargo target
// N10：并行发射下的 `.rodata` 内部化必须与串行发射**产出同一个 `.o`**。
//
// 缺陷本体（`docs/p12-physreg-spill-design.md` §11.13 的 N10 行 / 设计见 §11.33）：
// 函数级并行发射给每个 worker 下发一份 `rodata` + `string_pool` 快照，worker 只对
// **自己的快照**去重；主线程原先还把每个 chunk 的增量**整块追加**到全局 `rodata`。
// 于是跨 worker 重复的字符串（以及浮点常量）各存一份 —— `tests/fixtures/c/main.c`
// 的 `.rodata` 从 28B 变 56B，符号地址随之改变 ⇒ 整个 `.o` 与串行不同。
//
// 本文件的判据是**整个 `.o` 逐字节相同**（不是只看 `.text`，也不是只看符号项），
// 外加一条能一眼看出根因的指纹断言：被多个函数共用的字面量在 `.rodata` 里
// **只出现一次**。
//
// 防假绿：并行发射的准入条件是
// `parallel_codegen && relocatable && !emit_asm && top_funcs.len() >= 2`
// （见 `src/codegen/x86_64/emit/functions.rs`）。少一条就是拿串行比串行。所以
// 每个用例第一步都调 `n10_assert_parallel_eligible`，按 CLI 真值 + AST 里的
// 「带体顶层函数数」逐条核对；`..._eligibility_check_has_teeth` 反过来证明这条
// 核对确实会红（单函数 TU 必须被判为不合格）。
//
// 写在主题子文件里的约定：本文件不得出现 `use`/`mod`（导入权归壳），也不能用
// `//!` 内层文档注释（`include!` 是文本级粘贴，粘贴点不在模块开头，E0753）；
// 文件级助手一律 `pub(crate) fn` 而非 `fn` —— `scripts/gen_coverage_index.sh` 的
// STRUCT-HELPER 校验要求主题文件里 `^fn` 行数与测试属性行数相等，裸 `fn` 助手
// 会把这条守恒破坏掉（本文件因此对两条计数都是中性的）。

/// 用给定发射模式把 `source` 编成 `.o`，返回**整份**字节。
///
/// * `serial == true` ⇒ 设 `DCC_NO_PARALLEL=1`（串行发射）；否则显式从子进程环境
///   里移除该变量（并行发射），不让宿主 shell 的偶然设置影响判据；
/// * `launcher` 非空时是前置命令（如 `taskset -c 0`），用来压 worker 数。
pub(crate) fn n10_run(
    tag: &str,
    source: &std::path::Path,
    serial: bool,
    launcher: &[&str],
) -> Vec<u8> {
    n10_run_extra(tag, source, serial, launcher, &[])
}

/// `n10_run` 的「带额外 flag」版本：`-g` 那条用例要用它把调试信息也打开。
/// 单独开一份而不是改 `n10_run` 的签名，是为了让既有调用点的形状一字不变。
pub(crate) fn n10_run_extra(
    tag: &str,
    source: &std::path::Path,
    serial: bool,
    launcher: &[&str],
    extra: &[&str],
) -> Vec<u8> {
    let out_path = std::env::temp_dir().join(format!(
        "dcc-n10-{}-{}-{}.o",
        tag,
        if serial { "ser" } else { "par" },
        std::process::id()
    ));
    let mut cmd = if launcher.is_empty() {
        Command::new(env!("CARGO_BIN_EXE_dcc"))
    } else {
        let mut c = Command::new(launcher[0]);
        for a in &launcher[1..] {
            c.arg(a);
        }
        // 被 taskset 包住的那一项必须落在 dcc 二进制上，后面的 flag 才归 dcc。
        c.arg(env!("CARGO_BIN_EXE_dcc"));
        c
    };
    if serial {
        cmd.env("DCC_NO_PARALLEL", "1");
    } else {
        cmd.env_remove("DCC_NO_PARALLEL");
    }
    let result = cmd
        .arg("-O2")
        .args(extra)
        .arg("-c")
        .arg("-o")
        .arg(&out_path)
        .arg(source)
        .output()
        .expect("run dcc");
    let mode = if serial { "串行" } else { "并行" };
    assert!(
        result.status.success(),
        "[{tag}] {mode} 发射失败: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out_path).unwrap_or_else(|e| panic!("[{tag}] 读不到 {mode} .o: {e}"));
    let _ = fs::remove_file(&out_path);
    assert!(
        bytes.len() > 200,
        "[{tag}] {mode} .o 只有 {} 字节，太小，多半没走到发射",
        bytes.len()
    );
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F'], "[{tag}] {mode} 产物不是 ELF");
    bytes
}

/// 内联源 → 临时 `.c` → `n10_run`。返回整份 `.o` 字节。
pub(crate) fn n10_emit_src(tag: &str, src: &str, serial: bool, launcher: &[&str]) -> Vec<u8> {
    n10_emit_src_extra(tag, src, serial, launcher, &[])
}

/// `n10_emit_src` 的带额外 flag 版本。
pub(crate) fn n10_emit_src_extra(
    tag: &str,
    src: &str,
    serial: bool,
    launcher: &[&str],
    extra: &[&str],
) -> Vec<u8> {
    let src_path = std::env::temp_dir().join(format!("dcc-n10-{}-{}.c", tag, std::process::id()));
    fs::write(&src_path, src).expect("write N10 source");
    let bytes = n10_run_extra(tag, &src_path, serial, launcher, extra);
    let _ = fs::remove_file(&src_path);
    bytes
}

/// 只用 CLI 真值核对「这一行命令会走并行发射」——给不能直接拿 `dcc::parser::parse`
/// 吃的夹具源文件用（带 `#include` 的源未预处理时不是合法 AST，预处理后又带
/// 系统头的一堆方言）。函数数则由产物侧的**定义型 FUNC 符号数**兼做（见调用处）。
pub(crate) fn n10_assert_cli_parallel_enabled(tag: &str) {
    let args: Vec<String> = vec![
        "-O2".to_string(),
        "-c".to_string(),
        "-o".to_string(),
        format!("dcc-n10-{tag}.o"),
        format!("{tag}.c"),
    ];
    let cfg = dcc::cli::parse(&args).unwrap_or_else(|e| panic!("[{tag}] cli 解析失败: {}", e.msg));
    assert!(
        cfg.opt >= 2 && cfg.parallel_codegen,
        "[{tag}] -O2 下 parallel_codegen 没开（opt={}），并行路径没被走到",
        cfg.opt
    );
    assert!(cfg.relocatable, "[{tag}] 非 -c（ET_REL）：并行发射只覆盖可重定向产物");
    assert!(!cfg.emit_asm, "[{tag}] -S 汇编输出恒串行，不能用来验并行不变式");
}

/// 「这个 TU 真的会走分块并行发射吗」—— 与后端准入判据逐条对应。
/// 返回带体顶层函数数。任何一条不满足都必须让用例红，而不是静默变成串行比串行。
///
/// 注意 `parallel_codegen` 取自 CLI 真值：`opt >= 2 && DCC_NO_PARALLEL 未设`。若在
/// 跑测试的环境里设了 `DCC_NO_PARALLEL`，这里会红 —— 那是预期的，因为此时并行/串行
/// 两组产物其实是同一条路，比较本身没有意义（假绿）。
pub(crate) fn n10_assert_parallel_eligible(tag: &str, src: &str) -> usize {
    n10_assert_cli_parallel_enabled(tag);

    let (prog, diags) = dcc::parser::parse(src.as_bytes(), tag);
    assert!(
        !diags
            .iter()
            .any(|d| d.sev == dcc::diagnostics::Severity::Error),
        "[{tag}] 用例源码本身有错: {:?}",
        diags
            .iter()
            .filter(|d| d.sev == dcc::diagnostics::Severity::Error)
            .map(|d| (d.code, d.msg.clone()))
            .collect::<Vec<_>>()
    );
    let n = prog
        .decls
        .iter()
        .filter(|d| {
            matches!(d, dcc::ast::Decl::Func(f)
                if f.body.is_some() && f.alias_name.is_none() && f.error_msg.is_none())
        })
        .count();
    assert!(n >= 2, "[{tag}] 带体顶层函数只有 {n} 个：后端不会分块，并行==串行（假绿）",);
    n
}

/// `needle` 在 `hay` 里出现的次数（不重叠计数）。用来断言字面量只存了一份。
pub(crate) fn n10_count(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    let mut i = 0;
    let mut hits = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            hits += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    hits
}

/// 多个函数共用同一些字符串字面量 —— N10 的最小可复现形状。
///
/// `"shared"` 与 `"shared-message"` 是**前缀关系**：按内容键去重必须精确相等，
/// 任何「前缀匹配」式的实现会把其中一个吃掉，产物里少一份数据。
#[test]
fn parallel_rodata_shared_string_literals_match_serial_object() {
    let src = "\
extern int puts(const char *);
extern int strcmp(const char *, const char *);
int n10_a1(void) { puts(\"shared-message\"); return strcmp(\"shared\", \"shared-message\") + \"tail-string\"[0]; }
int n10_a2(void) { puts(\"shared-message\"); puts(\"tail-string\"); return \"shared\"[0]; }
int n10_a3(void) { puts(\"tail-string\"); puts(\"shared-message\"); return 0; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-str", src) >= 3);
    let par = n10_emit_src("par-rodata-str", src, false, &[]);
    let ser = n10_emit_src("par-rodata-str", src, true, &[]);

    // 指纹：合并期按内容键重放 ⇒ 全 TU 每份字面量只落一次。修复前这里是 3/3/2。
    let rod = elf64_section_data(&par, ".rodata").expect("并行 .rodata 不存在");
    let rod_ser = elf64_section_data(&ser, ".rodata").expect("串行 .rodata 不存在");
    for (needle, want) in [
        (b"shared-message\0".as_slice(), 1usize),
        (b"tail-string\0".as_slice(), 1),
        (b"shared\0".as_slice(), 1),
    ] {
        assert_eq!(
            n10_count(rod, needle),
            want,
            "并行 .rodata 里 {:?} 出现次数不对（N10 复发：跨 worker 各存一份）",
            std::str::from_utf8(needle)
        );
        assert_eq!(n10_count(rod_ser, needle), want, "串行基准也变了");
    }
    assert_eq!(rod, rod_ser, ".rodata 字节与串行不一致");
    assert_eq!(
        par,
        ser,
        "并行与串行的 .o 不逐字节相同：并行 {}B / 串行 {}B",
        par.len(),
        ser.len()
    );
}

/// 浮点常量走的是**按偏移**定址（`float_refs` / `x87_float_refs`），不是按内容定址。
/// 重放条目时必须同步换算这些 disp32，否则 SSE/x87 指令会读错常量。
#[test]
fn parallel_rodata_shared_float_and_x87_constants_match_serial() {
    let src = "\
extern double sin(double);
double n10_b1(void) { return sin(1.5) + sin(2.25); }
double n10_b2(void) { return sin(1.5) + sin(2.25) + sin(2.25); }
double n10_b3(void) { return sin(1.5) * sin(2.25); }
long double n10_b4(void) { return 3.75L + 3.75L; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-flt", src) >= 4);
    let par = n10_emit_src("par-rodata-flt", src, false, &[]);
    let ser = n10_emit_src("par-rodata-flt", src, true, &[]);

    // 1.5 的 f64 小端位型；出现多次就说明浮点常量按 worker 各存了一份。
    let d15: Vec<u8> = 1.5f64.to_le_bytes().to_vec();
    let rod = elf64_section_data(&par, ".rodata").expect("并行 .rodata 不存在");
    let rod_ser = elf64_section_data(&ser, ".rodata").expect("串行 .rodata 不存在");
    assert!(
        n10_count(rod_ser, &d15) >= 1,
        "基准（串行）里没有 f64 1.5 的位型，用例形状已经失效"
    );
    assert_eq!(
        n10_count(rod, &d15),
        n10_count(rod_ser, &d15),
        "并行下 f64 1.5 位型的份数与串行不同（按偏移引用的换算或去重失效）"
    );
    assert_eq!(rod, rod_ser, ".rodata 字节与串行不一致");
    assert_eq!(
        par,
        ser,
        "并行与串行的 .o 不逐字节相同：并行 {}B / 串行 {}B",
        par.len(),
        ser.len()
    );
}

/// `rodata_base != 0` 的那一类：字面量在**全局初始化阶段**就进了 `.rodata`，
/// worker 的快照前缀非空。此时 worker 绝对偏移 `< rodata_base` 的引用必须
/// **原值**落到最终偏移（旧实现无条件加 `rodata_delta`，会把这类引用推进增量里）。
#[test]
fn parallel_rodata_snapshot_prefix_offset_is_not_shifted() {
    let src = "\
extern int puts(const char *);
const char *n10_g = \"prefix-shared\";
double n10_c1(void) { puts(\"prefix-shared\"); return n10_g[0] + 1.25; }
double n10_c2(void) { puts(\"prefix-shared\"); return n10_g[1] + 1.25; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-pre", src) >= 2);
    let par = n10_emit_src("par-rodata-pre", src, false, &[]);
    let ser = n10_emit_src("par-rodata-pre", src, true, &[]);
    let rod = elf64_section_data(&par, ".rodata").expect("并行 .rodata 不存在");
    let rod_ser = elf64_section_data(&ser, ".rodata").expect("串行 .rodata 不存在");
    assert_eq!(
        n10_count(rod, b"prefix-shared\0"),
        1,
        "\"prefix-shared\" 没被前缀那份复用（快照基址换算失效）"
    );
    assert_eq!(rod, rod_ser, ".rodata 字节与串行不一致");
    assert_eq!(
        par,
        ser,
        "并行与串行的 .o 不逐字节相同：并行 {}B / 串行 {}B",
        par.len(),
        ser.len()
    );
}

/// 复合字面量走的是**不进池的裸追加**（还带对齐 pad）。这类字节必须原样搬运，
/// 而紧随其后的共用字符串仍要按内容去重 —— 两种形状混在同一段字节流里。
#[test]
fn parallel_rodata_compound_literal_and_shared_string_match_serial() {
    let src = "\
struct N10P { long x, y; };
extern int puts(const char *);
struct N10P n10_d1(void) { struct N10P p = (struct N10P){1, 2}; puts(\"dup-str\"); return p; }
struct N10P n10_d2(void) { struct N10P p = (struct N10P){1, 2}; puts(\"dup-str\"); return p; }
struct N10P n10_d3(void) { struct N10P p = (struct N10P){3, 4}; puts(\"dup-str\"); return p; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-cld", src) >= 3);
    let par = n10_emit_src("par-rodata-cld", src, false, &[]);
    let ser = n10_emit_src("par-rodata-cld", src, true, &[]);
    let rod = elf64_section_data(&par, ".rodata").expect("并行 .rodata 不存在");
    let rod_ser = elf64_section_data(&ser, ".rodata").expect("串行 .rodata 不存在");
    assert_eq!(
        n10_count(rod, b"dup-str\0"),
        1,
        "\"dup-str\" 跨 worker 存了多份（裸追加段的偏移记账漏了条目）"
    );
    assert_eq!(rod, rod_ser, ".rodata 字节与串行不一致");
    assert_eq!(
        par,
        ser,
        "并行与串行的 .o 不逐字节相同：并行 {}B / 串行 {}B",
        par.len(),
        ser.len()
    );
}

/// 任务书点名的真实夹具：修复前它的 `.rodata` 是 28B（串行）对 56B（并行）。
/// 这里不复制源码，直接对仓库里的夹具路径发射（`#include <stdio.h>` 的
/// 系统头搜索路径按仓库位置解析，抄到 /tmp 就换环境了）。
#[test]
fn parallel_rodata_fixture_main_c_is_byte_identical() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c/main.c");
    assert!(fs::metadata(&src).is_ok(), "夹具不存在: {}", src.display());
    // 带 `#include` 的夹具不能直接丢给 parser（没预处理），所以准入分成两半：
    // CLI 侧逐条核对并行开关，函数数则拿产物里定义型 FUNC 符号数当读数
    //（一函数一 chunk，两个定义就是两个 chunk，等价于 `top_funcs.len() >= 2`）。
    n10_assert_cli_parallel_enabled("par-rodata-main");
    let par = n10_run("par-rodata-main", &src, false, &[]);
    let ser = n10_run("par-rodata-main", &src, true, &[]);
    let funcs = elf64_func_symbols(&par);
    assert!(
        funcs.iter().filter(|(_, _, size)| *size > 0).count() >= 2,
        "并行 .o 里只有 {} 个定义型 FUNC 符号：本 TU 不会分块，并行==串行（假绿）",
        funcs.len()
    );
    assert_eq!(
        funcs
            .iter()
            .map(|(n, _, s)| (n.clone(), *s))
            .collect::<Vec<_>>(),
        elf64_func_symbols(&ser)
            .iter()
            .map(|(n, _, s)| (n.clone(), *s))
            .collect::<Vec<_>>(),
        "并行与串行的函数符号表（名/尺寸）不同"
    );
    let rod = elf64_section_data(&par, ".rodata").expect("并行 .rodata 不存在");
    let rod_ser = elf64_section_data(&ser, ".rodata").expect("串行 .rodata 不存在");
    assert_eq!(
        rod.len(),
        rod_ser.len(),
        ".rodata 尺寸并行 {}B / 串行 {}B（N10 的原始症状）",
        rod.len(),
        rod_ser.len()
    );
    assert_eq!(
        n10_count(rod, b"sum=%d sq=%d ctr=%d loc=%d\n\0"),
        1,
        "main.c 的 printf 格式串被存了多份"
    );
    assert_eq!(
        par,
        ser,
        "并行与串行的 .o 不逐字节相同：并行 {}B / 串行 {}B",
        par.len(),
        ser.len()
    );
}

/// 确定性：产物不得依赖 worker 数 / 调度。
///
/// `taskset -c 0` 把 `available_parallelism()` 压到 1，后端走 `degree <= 1` 分支 ——
/// 那仍然是「一函数一 chunk + 按 slot 序合并」的同一条路（所以单核机器也在判据覆盖
/// 内），只是并发度不同。三种度数的产物必须两两相同。没有 `taskset` 的环境跳过。
#[test]
fn parallel_rodata_object_independent_of_worker_degree() {
    let src = "\
extern int puts(const char *);
extern int strcmp(const char *, const char *);
int n10_e1(void) { puts(\"deg-shared\"); return strcmp(\"deg-shared\", \"tail-deg\")[0]; }
int n10_e2(void) { puts(\"deg-shared\"); puts(\"tail-deg\"); return 1; }
int n10_e3(void) { puts(\"tail-deg\"); return 2; }
int n10_e4(void) { puts(\"deg-shared\"); return 3; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-deg", src) >= 4);
    if Command::new("taskset").arg("--version").output().is_err() {
        eprintln!("无 taskset，跳过 worker 数无关性检查（其余判据已覆盖逐字节相同）");
        return;
    }
    let ser = n10_emit_src("par-rodata-deg", src, true, &[]);
    let par_many = n10_emit_src("par-rodata-deg", src, false, &[]);
    let par_one = n10_emit_src("par-rodata-deg", src, false, &["taskset", "-c", "0"]);
    let par_two = n10_emit_src("par-rodata-deg", src, false, &["taskset", "-c", "0,1"]);
    assert_eq!(par_one, ser, "degree=1（分块串行跑）与串行的 .o 不同：合并期重放不成立");
    assert_eq!(par_two, ser, "degree=2 与串行的 .o 不同");
    assert_eq!(par_many, ser, "默认 degree 与串行的 .o 不同");
}

/// 确定性（另一半）：同一条并行路反复跑，字节必须一致。
/// `pool_add` 来自 `HashMap` 迭代，若不按 (偏移, 键) 升序归并，重放顺序会随运行
/// 变化 —— 这条用例就是把「跑多次」变成判据（也是 E1104 的随机性来源检查）。
#[test]
fn parallel_rodata_object_reproducible_across_repeated_parallel_runs() {
    let src = "\
extern int puts(const char *);
int n10_f1(void) { puts(\"rep-a\"); puts(\"rep-b\"); return 0; }
int n10_f2(void) { puts(\"rep-b\"); puts(\"rep-c\"); return 1; }
int n10_f3(void) { puts(\"rep-a\"); puts(\"rep-c\"); puts(\"rep-d\"); return 2; }
int n10_f4(void) { puts(\"rep-d\"); return 3; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-rep", src) >= 4);
    let baseline = n10_emit_src("par-rodata-rep", src, false, &[]);
    for round in 1..=4 {
        let again = n10_emit_src("par-rodata-rep", src, false, &[]);
        assert_eq!(baseline, again, "第 {round} 轮并行产物与前一轮不同（不可复现）");
    }
    assert_eq!(
        baseline,
        n10_emit_src("par-rodata-rep", src, true, &[]),
        "并行与串行的 .o 不逐字节相同"
    );
}

/// 反向自检：上面那套准入核对确实会红，而不是永远返回「合格」。
///
/// 没有这条，`n10_assert_parallel_eligible` 一旦写错（比如漏了函数数判据），
/// 整个文件的用例就会悄悄退化成「串行比串行」。单带体函数的 TU 必须被判不合格。
#[test]
fn parallel_rodata_parallel_eligibility_check_has_teeth() {
    let solo = "int n10_solo(void) { return 42; }\n";
    let panicked = std::panic::catch_unwind(|| {
        n10_assert_parallel_eligible("n10-teeth", solo);
    })
    .is_err();
    assert!(panicked, "单函数 TU 被判为可并行：准入核对失效（用例会变成假绿）");

    let pair = "\
extern int puts(const char *);
int n10_p1(void) { puts(\"teeth\"); return 1; }
int n10_p2(void) { puts(\"teeth\"); return 2; }
";
    assert_eq!(
        n10_assert_parallel_eligible("n10-teeth-ok", pair),
        2,
        "两函数 TU 的带体函数数核对不对"
    );

    // 声明（无体）不计入分块：只写原型不该被当成「真的并行」。
    let decls_only = "\
extern int puts(const char *);\nint n10_proto(void);\nint n10_impl(void) { puts(\"x\"); return 0; }\n";
    let panicked =
        std::panic::catch_unwind(|| n10_assert_parallel_eligible("n10-teeth-proto", decls_only))
            .is_err();
    assert!(panicked, "只有 1 个带体函数 + 1 个原型就被判为可并行：函数数判据数错了");
}

/// `-g` 构建里 N10 面同样必须逐字节相同 —— 把「整个 `.o`」判据延伸到带调试信息的产物。
///
/// 为什么不能只看不带 `-g` 的那一对：`check_parallel_vs_serial.sh` 早期在 `-g` 层只比
/// `.text` + `.debug_info`，于是 `.rodata` 的重复副本在 `-g` 下没人看（实测 HEAD 二进制
/// 在 `-g` 下有 44 个夹具的 `.rodata` 不同，本文件最后那条 `secs:` 明细就是这个形状）。
///
/// 反向说清楚：`-g` 还剩一处**与本缺陷无关**的缺口 —— `.debug_line` / `.rela.debug_line`
/// / `.BTF.ext` 三节（实测 158/293 个夹具参与，且只有这三节）。根因是 `FuncEmitChunk`
/// 没有带回 `Codegen::{stmt_line_entries, asm_line_entries, asm_insn_debug_entries}`
/// 三张行表。修它要给 `codegen.rs` 里的 chunk 结构加 debug 面字段并动 `src/debug.rs`，
/// 两者都在本轮禁改清单里 ⇒ 本用例**只**断言非 debug/BTF 节，并显式跳过那三节
/// （口径与数字见 `docs/p12-physreg-spill-design.md` §11.33）。
#[test]
fn parallel_rodata_g_build_non_debug_sections_match_serial() {
    let src = "\
extern int puts(const char *);
int n10_c1(void) { puts(\"g-shared-str\"); return \"g-tail\"[0]; }
int n10_c2(void) { puts(\"g-shared-str\"); puts(\"g-tail\"); return 0; }
int n10_c3(void) { puts(\"g-tail\"); puts(\"g-shared-str\"); return 1; }
";
    assert!(n10_assert_parallel_eligible("par-rodata-g", src) >= 3);
    let par = n10_emit_src_extra("par-rodata-g", src, false, &[], &["-g"]);
    let ser = n10_emit_src_extra("par-rodata-g", src, true, &[], &["-g"]);

    // 1) 正题：字面量各存一份在 `-g` 下不构成豁免。
    let rod = elf64_section_data(&par, ".rodata").expect("并行(-g) .rodata 不存在");
    let rod_ser = elf64_section_data(&ser, ".rodata").expect("串行(-g) .rodata 不存在");
    assert_eq!(rod, rod_ser, "-g 下 .rodata 与串行不一致（N10 在 -g 构建里复发）");
    for needle in [b"g-shared-str\0".as_slice(), b"g-tail\0".as_slice()] {
        assert_eq!(
            n10_count(rod, needle),
            1,
            "-g 下 {:?} 被存了多份（跨 worker 未去重）",
            std::str::from_utf8(needle)
        );
    }
    // 2) 机器码：`-g` 不得改变指令选择（N1/N12 那一族）。
    assert_eq!(
        elf64_section_data(&par, ".text"),
        elf64_section_data(&ser, ".text"),
        "-g 下 .text 与串行不一致"
    );
    // 3) 除 debug/BTF/note 面以外的**所有**节：内容与节集合都必须相同。
    //    `.shstrtab` 也跳过：它列出的是节名集合，而那三节的有无本身属 debug 面缺口。
    let sp = elf64_sections(&par);
    let ss = elf64_sections(&ser);
    let skipped = |n: &str| {
        n.starts_with(".debug")
            || n.starts_with(".rela.debug")
            || n.starts_with(".BTF")
            || n.starts_with(".note")
            || n == ".shstrtab"
    };
    let mut checked = 0usize;
    for name in sp.keys() {
        if skipped(name.as_str()) {
            continue;
        }
        assert!(ss.contains_key(name), "-g 并行有节 {name}，串行没有：节集合不一样");
        assert_eq!(
            elf64_section_data(&par, name),
            elf64_section_data(&ser, name),
            "-g 下节 {name} 的内容与串行不同"
        );
        checked += 1;
    }
    for name in ss.keys() {
        if skipped(name.as_str()) {
            continue;
        }
        assert!(sp.contains_key(name), "-g 串行有节 {name}，并行没有：节集合不一样");
    }
    assert!(checked >= 5, "只比了 {checked} 个非 debug 节，多半压根没走到 -g 构建");
}
