// tests/reproducibility.rs - 编译产物可复现性回归锁
//
// 为什么单独成文件 (而不是塞进 tests/integration.rs):
//   integration.rs 已 23k 行, 且它的关注点是"编出来的语义对不对"。本文件守的是
//   另一条独立性质 —— "同一输入连跑多次, 字节是否全等"。这条性质在 -O2 上失守过
//   (P0-d「构建不可复现」): SSA 管线把 `original_locals: HashSet<String>` 的
//   **进程随机迭代序**写进重建 AST 的局部变量声明序, 后端再按声明序分配栈槽,
//   于是 `.text` 里出现 `lea -72(%r13)` ↔ `lea -80(%r13)` 的单字节抖动。
//
// 两条锁的分工:
//   1) CLI 多进程锁 (`*_byte_reproducible`): 每轮 `Command::new` 都是一个**新进程**,
//      `std::collections::{HashMap, HashSet}` 的 RandomState 种子按进程播种, 因此只有
//      跨进程才能暴露哈希序泄漏。这是端到端的最终判据。
//   2) 库内不变量锁 (`rebuild_block_uses_source_declaration_order`): 直接断言
//      SSA→AST 重建发出的局部声明序 == 源码声明序。它不依赖后端偏移细节, 泄漏
//      时能一步指到 rebuild_block, 而不是让人去 diff 二进制。

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// 最小仍抖用例 (从 tests/fixtures/c/stmt_expr.c 削减而来)。
///
/// 3 个局部变量都要被取址, 这样它们的栈槽偏移一定会出现在 `.text` 的 `lea` 里;
/// 函数不含循环, 因此 -O2 会走 `run_ssa_passes` 的 SSA→AST 往返 (含循环的函数
/// 目前会被跳过, 泄漏面不同)。
/// 在修复前的二进制上, 连跑 5 次可观察到 4 种不同的字节输出。
const JITTER_SOURCE: &str = r#"
extern int side_effect(int *p);
int repro(void) {
    int fail = 0;
    int x = 10;
    int y = 20;
    if (side_effect(&x) != 30) fail |= 32;
    if (side_effect(&y) != 40) fail |= 64;
    return fail;
}
"#;

fn write_tmp(name: &str, content: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("dcc-repro-{name}-{}.c", std::process::id()));
    fs::write(&path, content).expect("write reproducibility fixture");
    path
}

/// 用同一二进制、同一输入连跑 `runs` 次, 返回每次产物的字节。
///
/// 关键: 每次都重新 `Command::new`, 拿到的是全新的进程 / 全新的哈希种子。
///
/// `tag` 必须由每个测试单独提供: cargo 默认并行跑测试且它们共用同一个 pid,
/// 临时产物名若只带 pid 会相互踩文件 (曾因此把 A 测试的 .o 读进 B 测试的断言)。
fn compile_runs(src: &PathBuf, args: &[&str], tag: &str, runs: usize) -> Vec<Vec<u8>> {
    let mut outputs = Vec::with_capacity(runs);
    for i in 0..runs {
        let out =
            std::env::temp_dir().join(format!("dcc-repro-{}-{tag}-{i}.out", std::process::id()));
        let status = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .arg("--target")
            .arg("x86_64-linux-gnu")
            .args(args)
            .arg(src)
            .arg("-o")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run dcc");
        assert!(status.success(), "dcc {args:?} 编译失败 (exit {status:?})");
        let bytes = fs::read(&out).unwrap_or_else(|e| panic!("读取 {out:?} 失败: {e}"));
        let _ = fs::remove_file(&out);
        outputs.push(bytes);
    }
    outputs
}

/// 断言所有字节全等, 失败时报告"第几次、第几个字节开始不同"。
fn assert_all_identical(outputs: &[Vec<u8>], what: &str) {
    assert!(outputs.len() >= 2, "至少要有两次运行才有可比性");
    let base = &outputs[0];
    for (i, out) in outputs.iter().enumerate().skip(1) {
        assert_eq!(
            out.len(),
            base.len(),
            "{what}: 第 {} 次运行产物长度 {} != 首次 {}",
            i + 1,
            out.len(),
            base.len()
        );
        if let Some(pos) = out.iter().zip(base.iter()).position(|(a, b)| a != b) {
            panic!(
                "{what}: 第 {} 次运行与首次逐字节不同, 首个差异在偏移 {:#x} \
                 (new={:#04x} old={:#04x}) —— 典型症状是栈槽/节序泄漏",
                i + 1,
                pos,
                out[pos],
                base[pos]
            );
        }
    }
}

#[test]
fn o2_object_is_byte_reproducible() {
    let src = write_tmp("o2c", JITTER_SOURCE);
    let runs = compile_runs(&src, &["-O2", "-c"], "o2c", 8);
    let _ = fs::remove_file(&src);
    assert_all_identical(&runs, "-O2 -c 产物");
}

#[test]
fn o3_object_is_byte_reproducible() {
    let src = write_tmp("o3c", JITTER_SOURCE);
    let runs = compile_runs(&src, &["-O3", "-c"], "o3c", 8);
    let _ = fs::remove_file(&src);
    assert_all_identical(&runs, "-O3 -c 产物");
}

#[test]
fn o2_assembly_is_byte_reproducible() {
    let src = write_tmp("o2s", JITTER_SOURCE);
    // -S 走同一份重建 AST, 泄漏在这里表现为文本行内容 (偏移常量) 抖动。
    let runs = compile_runs(&src, &["-O2", "-S"], "o2s", 8);
    let _ = fs::remove_file(&src);
    assert_all_identical(&runs, "-O2 -S 产物");
}

#[test]
fn o2_with_debuginfo_is_byte_reproducible() {
    let src = write_tmp("o2g", JITTER_SOURCE);
    // "-g -O2 -c" 同时覆盖 DWARF 里的局部变量偏移 (DW_OP_fbreg), 它是第二个
    // 会把栈槽序写进产物的地方。
    let runs = compile_runs(&src, &["-g", "-O2", "-c"], "o2g", 8);
    let _ = fs::remove_file(&src);
    assert_all_identical(&runs, "-g -O2 -c 产物");
}

/// -O0 / -O1 早已确定, 但它们是"零回归基线": 任何为 -O2 确定性所做的改动都不
/// 得顺手改掉这两档的字节序判定, 所以这里同样上锁。
#[test]
fn o0_and_o1_stay_byte_reproducible() {
    let src = write_tmp("o01", JITTER_SOURCE);
    for level in ["-O0", "-O1"] {
        let runs = compile_runs(&src, &[level, "-c"], "o", 5);
        assert_all_identical(&runs, &format!("{level} -c 产物"));
    }
    let _ = fs::remove_file(&src);
}

/// 链接模式（不带 `-c`）的最小可链接输入。
///
/// 与 JITTER_SOURCE 不同，这里必须自带 `main` 且不引用任何未定义符号：
/// host-link 会把临时 .o 交给宿主 `cc` 真链接，缺 `main` 或未解析符号会在
/// **链接阶段**就失败，测不到「产物字节是否稳定」这一层。
///
/// 为什么必须带 `static`（实测结论，删掉它这条锁会变假绿）：宿主 ld 只在一个
/// 输入 .o **含文件作用域（LOCAL）符号**时，才为它登记 STT_FILE 符号；全 GLOBAL 的
/// TU 不触发，于是临时 .o 名根本不会进产物，连跑恒等——那等于没测。
/// 实测（同一份源码，修复前的二进制）：
///   * 只有 GLOBAL 函数    -> 产物里 `grep -c 'dcc-hostlink-[0-9]'` = 0, 3 次跑 1 个哈希
///   * 加 static 函数/变量  -> 命中 = 1, 3 次跑 3 个哈希（`dcc-hostlink-3790994-s2.c.o`）
const LINK_SOURCE: &str = r#"
static int side_effect(int *p) { return *p + 20; }
static int trace;
int repro(void) {
    int fail = 0;
    int x = 10;
    int y = 20;
    trace = 1;
    if (side_effect(&x) != 30) fail |= 32;
    if (side_effect(&y) != 40) fail |= 64;
    return fail + trace;
}
int main(void) { return repro(); }
"#;

/// 链接模式专用：源文件路径**不带 PID**。
///
/// 为什么不用 `write_tmp`：host-link 的临时 .o 名是按设计拼上输入文件名的
/// （`dcc-hostlink-<src>.o`，宿主 ld 会把它当 STT_FILE 登进 .symtab），而
/// `write_tmp` 的名字里就带 PID —— 那会让下面那条「名字里不得出现 PID 形状」
/// 的断言自我矛盾。 cargo 同进程并行跑测试时本路径只被本测试写，不冲突。
fn write_tmp_stable(content: &str) -> PathBuf {
    let path = std::env::temp_dir().join("dcc-repro-linksrc-stable.c");
    fs::write(&path, content).expect("write link-mode reproducibility fixture");
    path
}

/// 断言产物里不存在 `dcc-hostlink-<数字>...` 这种名字（即 PID 泄漏）。
///
/// 为什么不光靠「连跑 N 次字节全等」：那只换掉泄漏量就能过（用随机数代替 PID
/// 照样全等，因为本测试跑在同一进程里），而它抓的就是历史上真实出现过的那个
/// 形状 `dcc-hostlink-{std::process::id()}-{src}.o`。证伪已实测：把本文件编给
/// 修复前的二进制，这条断言会因产物 `.strtab`（偏移 0x267e）里出现的
/// `dcc-hostlink-<pid>-dcc-repro-linksrc-stable.c.o` 而失败；连跑 3 次得 3 个不同哈希。
fn assert_no_pid_shaped_hostlink_name(bytes: &[u8], what: &str) {
    const PAT: &[u8] = b"dcc-hostlink-";
    let mut from = 0usize;
    while let Some(rel) = bytes[from..].windows(PAT.len()).position(|w| w == PAT) {
        let at = from + rel + PAT.len();
        let next = bytes.get(at).copied().unwrap_or(b'\0');
        assert!(
            !next.is_ascii_digit(),
            "{what}: 产物里出现 `dcc-hostlink-<数字>` 形状的名字（偏移 {:#x}，下一字节 {:?}）\
             —— 宿主 ld 会把它写成 STT_FILE，临时 .o 名里的易变量就直接泄进可执行文件",
            from + rel,
            next as char
        );
        from = at;
    }
}

/// 链接模式（产出可执行文件）的跨进程字节回归锁。
///
/// 为什么需要单独一条：前面几条都是 `-c`/`-S`，根本不经过
/// `driver.rs::run_with_host_link`。那条路径会先编出一个临时 .o 再交给宿主
/// `cc`，而它的名字曾是 `dcc-hostlink-<pid>-<src>.o` —— 宿主 ld 把 basename 写成
/// STT_FILE 送进 `.symtab/.strtab`，于是 PID 进了产物。与优化档无关，-O0 也一样抖。
/// 实测（同一输入连跑 5 次）：修复前 5 个不同 sha256，修复后 1 个。
#[test]
fn linked_executable_is_byte_reproducible() {
    // host-link 本质需要宿主链接器；没 `cc` 时这个性质不可观测，直接记为跳过
    // （而不是假装通过）——这里跳过会打印警告，与断言失败区分。
    if !Command::new("cc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        eprintln!("SKIP: 本机没有可用的宿主 `cc`，链接模式回归锁不适用");
        return;
    }
    let src = write_tmp_stable(LINK_SOURCE);
    let runs = compile_runs(&src, &["-O0"], "link", 5);
    let _ = fs::remove_file(&src);
    assert_all_identical(&runs, "链接模式 (-O0, 无 -c) 可执行文件");
    for bytes in &runs {
        assert_no_pid_shaped_hostlink_name(bytes, "链接模式可执行文件");
    }
}

/// 直接锁定根因处的不变量: `rebuild_block` 发出的局部变量声明必须保持**源码声明序**。
///
/// 为什么在单进程里也能抓到泄漏: `std` 的 `RandomState` 每 new 一个随机 hasher
/// 都会推进进程内种子, 所以循环 30 轮 (每轮重新 parse 出全新的 `HashSet`) 时,
/// 修复前的实现必然至少在某一轮给出与源码序不同的排列。
#[test]
fn rebuild_block_uses_source_declaration_order() {
    use dcc::ast::{BlockItem, Decl};

    const MULTI_LOCAL_SOURCE: &str = r#"
extern void use_(int *p);
int many(void) {
    int alpha = 1;
    int bravo = 2;
    int charlie = 3;
    int delta = 4;
    int echo = 5;
    int foxtrot = 6;
    use_(&alpha); use_(&bravo); use_(&charlie);
    use_(&delta); use_(&echo); use_(&foxtrot);
    return alpha + foxtrot;
}
"#;

    // 跨轮基准序：只在第一轮定下来，后面每一轮都拿它比。
    let mut reference: Option<Vec<String>> = None;

    let expected_locals: Vec<&str> = vec!["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];

    for round in 0..30 {
        let (prog, diags) = dcc::parser::parse(MULTI_LOCAL_SOURCE.as_bytes(), "many.c");
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.sev, dcc::diagnostics::Severity::Error)),
            "round {round}: 测试源码不应有错误: {:?}",
            diags.iter().map(|d| d.msg.clone()).collect::<Vec<_>>()
        );
        let f = prog
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == "many" => Some(f),
                _ => None,
            })
            .expect("test source must define `many`");
        let ssa = dcc::ssa::build_from_func(f).expect("`many` 是结构化函数, SSA 构造应成功");
        let rebuilt = dcc::ssa::rebuild_block(&ssa);

        let emitted: Vec<String> = rebuilt
            .items
            .iter()
            .filter_map(|item| match item {
                BlockItem::Decl(Decl::Var(v)) => Some(v.name.name.clone()),
                _ => None,
            })
            .collect();

        // 本测试守的是两件事，都不是「一个临时值也不能多」：
        //   1. **用户声明的局部**必须按源码声明序出现在最前面（少了、换了都是
        //      `HashSet` 迭代序泄漏，会直接改写栏位偏移 ⇒ -O2 产物不可复现）；
        //   2. 30 轮跑出的序必须逐字相同（序一旦取决于哈希序就会在这里暴露）。
        //
        // 后面允许有合成临时值：取址类临时值**不得**被内联进取址操作数（否则重建出
        // `&(&m[i])[j]` 这种嵌套取址，从错值恶化成段错误；判据见 `is_addr_producer`）。
        // 所以旧版那句「emitted 恰好等于 6 个用户局部」已经不再是要守的性质 ——
        // 把它换成下面三条：前缀 = 源码序、余下全是 `_` 前缀的合成值、30 轮恒等。
        let head: Vec<&str> = emitted
            .iter()
            .take(expected_locals.len())
            .map(String::as_str)
            .collect();
        assert_eq!(
            head,
            expected_locals,
            "round {round}: rebuild_block 的前 {n} 条局部声明必须是源码声明序（实际 {emitted:?}）",
            n = expected_locals.len()
        );
        for extra in &emitted[expected_locals.len()..] {
            assert!(
                extra.starts_with('_'),
                "round {round}: 用户局部之后只能出现编译器合成临时值，实际多了一个 {extra:?}（全部：{emitted:?}）"
            );
        }
        if round == 0 {
            // 基准序：后面每一轮都拿它比，才抓得住「同一输入两次编译序不同」。
            reference = Some(emitted.clone());
        } else {
            assert_eq!(
                reference.as_ref().unwrap(),
                &emitted,
                "round {round}: 局部声明序在两次重建之间变了（不可复现的直接证据）"
            );
        }
    }
}

/// 并行发射不得改产物：同一份源在默认（并行）与 `DCC_NO_PARALLEL=1`（串行）下
/// 的 `.o` 必须**逐字节相同**。
///
/// 这条性质失守过两次，而且是同一类根因：**程序级事实没有随下行快照走**。
/// * N1 —— worker 不回传 `defined_funcs`/`func_sizes`，嵌套函数的唯一名在 ELF 侧发成 UND；
/// * N12 —— worker 重建 `Codegen` 时把全局的 `ty` 占位成 `Type::Int`、`init` 占位成
///   `None`，并且根本不下发 `static_local_map`。实测同一个 `.o` 的 `.text` 在两种模式下
///   差 691 字节：`movzbl`（串行，`unsigned char`/`_Bool` 正确）变成 `movsbl`（并行），
///   而带静态局部的函数走了「懒初始化 + 帧内槽」的退化路，直接长了一截。
///   反向对照已验：修复前的二进制在本用例上红（`.text` 691 字节不同）。
///
/// 判据要「整文件相同」而不是「只看 `.text`」，所以夹具刻意不含字符串/浮点字面量 ——
/// N10（并行 `.rodata` 按 worker 内部化）会改 `.rodata` 与符号地址，那还挂着，
/// 见 `docs/p12-physreg-spill-design.md` §11.13 的 N10 行与 §11.24。
#[test]
fn parallel_codegen_preserves_program_level_facts() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c/parallel_global_facts.c");
    let exists = fs::metadata(&src).is_ok();
    assert!(exists, "夹具不存在：{}", src.display());

    let emit = |serial: bool, tag: &str| -> Vec<u8> {
        let out =
            std::env::temp_dir().join(format!("dcc-repro-par-{tag}-{}.o", std::process::id()));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
        cmd.arg("--target")
            .arg("x86_64-linux-gnu")
            .arg("-O2")
            .arg("-c")
            .arg("-o")
            .arg(&out)
            .arg(&src);
        if serial {
            cmd.env("DCC_NO_PARALLEL", "1");
        }
        let status = cmd.output().expect("run dcc");
        assert!(
            status.status.success(),
            "{tag} 编译失败: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let bytes = fs::read(&out).expect("read .o");
        let _ = fs::remove_file(&out);
        bytes
    };

    // 并行路径要求「至少两个带体的顶层函数」才会真的分块，所以先确认本用例真的走并行；
    // 否则下面这句只是把串行和串行比，本用例就成了假绿。
    let par = emit(false, "par");
    let ser = emit(true, "ser");
    assert!(!par.is_empty(), "并行 .o 为空（假绿）");
    assert!(par.len() > 200, "并行 .o 只有 {} 字节，太小，多半没走到分块发射", par.len());
    assert_eq!(
        par,
        ser,
        "并行与串行的 .o 不逐字节相同（并行改了产物）：并行 {}B / 串行 {}B",
        par.len(),
        ser.len()
    );
}
