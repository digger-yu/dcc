//! 集成测试公共 harness：编译/运行/ELF 解析/C++ 交叉对照等跨主题复用助手。
//!
//! 由壳 `tests/integration.rs` 以 `#[path = "common/harness.rs"] mod harness;` 挂载，
//! 再 `use harness::*;` 注入各主题子文件（子文件自身不得写 `use`/`mod`）。条目一律
//! `pub(crate)`：integration target 是独立 crate，`pub(crate)` 与 `pub` 效果相同，但
//! 显式收敛可见面不对外承诺 API。体量自成一体的两组助手分入子模块后重导出：
//! `harness/elf.rs`（ELF64 解析）、`harness/{driver_cli,cpp_frontend}.rs`（驱动 CLI 与 C++ 前端）。
//! 因本文件由 `#[path]` 挂载（子模块默认解析落到 `tests/common/`），需显式写前缀。

#[path = "harness/driver_cli.rs"]
mod driver_cli;
#[path = "harness/cpp_frontend.rs"]
mod cpp_frontend;
#[path = "harness/elf.rs"]
mod elf;
// 单入口重导出：同一 harness 被多个 target 共用（`integration` 与 `profile`），
// 每个 target 只消费其中一部分属预期设计，所以在此声明点放宽 unused_imports；
// 真正未使用的条目仍由 `dead_code` 在各 target 的挂载点上把关。
#[allow(unused_imports)]
pub(crate) use cpp_frontend::*;
pub(crate) use driver_cli::*;
pub(crate) use elf::*;

use std::fs;
use std::process::Command;

pub(crate) fn run_syntax_test(name: &str, source: &str) -> std::process::Output {
    let path = std::env::temp_dir().join(format!("dcc-{name}-{}.c", std::process::id()));
    fs::write(&path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(path);
    output
}


pub(crate) fn run_object_test(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
    run_object_test_flags(name, source, &[])
}


pub(crate) fn run_object_test_flags(
    name: &str,
    source: &str,
    extra_flags: &[&str],
) -> (std::process::Output, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-{name}-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-c");
    for flag in extra_flags {
        cmd.arg(flag);
    }
    let output = cmd
        .arg("-o")
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    (output, object_path)
}


pub(crate) fn run_asm_output_test(name: &str, source: &str) -> (std::process::Output, String) {
    run_asm_output_test_flags(name, source, "")
}

/// 同一件事，但可以额外传一组 flags（按空白分开）。
///
/// 为什么需要：有些性质只在特定档位上成立/失效。实例：`__seg_gs` 的 pointee 段前缀
/// 在 -O0 下发得对，-O2 的 SSA 往返会把段信息弄丢（只在部分访问形态上发）—— 只看
/// 默认档位就抓不到。空串时行为与 `run_asm_output_test` 一字不差。
pub(crate) fn run_asm_output_test_flags(
    name: &str,
    source: &str,
    extra_flags: &str,
) -> (std::process::Output, String) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-{name}-{}.s", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    if !extra_flags.is_empty() {
        cmd.args(extra_flags.split_whitespace());
    }
    let output = cmd
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    (output, asm_text)
}


/// 编译为可执行文件并返回路径（Linux 远程测试用）
pub(crate) fn run_compile_and_run(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
    run_compile_and_run_flags(name, source, &[])
}


/// 带额外参数的编译并运行
pub(crate) fn run_compile_and_run_flags(
    name: &str,
    source: &str,
    extra_flags: &[&str],
) -> (std::process::Output, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let exe_path = dir.join(format!("dcc-{name}-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.args(["-std=gnu23"]);
    for flag in extra_flags {
        cmd.arg(flag);
    }
    cmd.args(["-o"]).arg(&exe_path).arg(&source_path);
    let output = cmd.output().expect("run dcc");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
    }
    let _ = fs::remove_file(source_path);
    (output, exe_path)
}


/// 运行上限时间 (秒), 由 `DCC_TEST_EXE_TIMEOUT_SECS` 覆盖, 默认 20。
const EXE_TIMEOUT_DEFAULT: u64 = 20;

fn exe_timeout_secs() -> u64 {
    std::env::var("DCC_TEST_EXE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(EXE_TIMEOUT_DEFAULT)
}

/// **有界**地运行一个可执行文件, 返回 (退出码, stdout, stderr)。
///
/// 为什么必须由这一个函数管: 优化器最危险的失效形态是静默误编译出来的**死循环**
/// (`-O2` 的 LICM 曾把整个 while 体外提成空转)。而 `Command::output()` 会无限阻塞,
/// 于是这种缺陷不是把测试跑红, 而是把整个 `cargo test` 吊死 —— 门禁看起来“还在跑”,
/// 实际永远不给出结论。超时一律当失败 (panic), 让缺陷以红色测试的形式可定位。
///
/// 读两个管道在单独线程里 `read_to_end`: 不在主线程排空的话, 子进程写满 pipe 缓冲区
/// 会阻塞在 write 上, `try_wait` 永远拿不到退出状态 → 正常程序被误判为挂死。
pub(crate) fn run_exe_bounded(path: &std::path::Path) -> (i32, Vec<u8>, Vec<u8>) {
    let mut child = Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("run {}: {e}", path.display()));
    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");
    let out_join = std::thread::spawn(move || {
        let mut v: Vec<u8> = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out, &mut v);
        v
    });
    let err_join = std::thread::spawn(move || {
        let mut v: Vec<u8> = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err, &mut v);
        v
    });
    let limit = exe_timeout_secs();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(limit);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(st) => break st,
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // 等排空线程收尾, 否则它们会拿着已关闭的 fd 继续重试。
                    let _ = out_join.join();
                    let _ = err_join.join();
                    panic!(
                        "HANG: {} 未在 {limit}s 内退出 (很可能是优化器造成的死循环误编译)",
                        path.display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
        }
    };
    let stdout = out_join.join().expect("stdout reader thread");
    let stderr = err_join.join().expect("stderr reader thread");
    (status.code().unwrap_or(-1), stdout, stderr)
}


pub(crate) fn run_exe(path: &std::path::Path) -> i32 {
    run_exe_bounded(path).0
}


/// GCC 交叉验证: 同一份 C 源分别用 `gcc -O0` 与 dcc 编译链接并运行, 要求
/// **退出码相同**且 **stdout 逐字节一致**, 然后返回该退出码供调用方再断言具体值。
///
/// 为什么变参 ABI 用例必须这样测: 只断言"dcc 产物返回 0"时, caller 与 callee
/// 若把某一类参数错放到同一个位置 (两侧同错), 夹具依然自洽通过 —— 而这正是
/// ABI 互操作性最坏的失效形态。外部权威实现 (gcc) 的逐字节输出对照能把这类
/// "内部一致但与世不符"的错误照出来。
///
/// 注: 本机系统 gcc 尚无 `-std=gnu23`, 用 `-std=gnu2x` 对应 dcc 的默认 `gnu23`;
/// 两边链接都用主机 cc, 因而 libc 的 `printf` 格式化行为一致。
pub(crate) fn run_gcc_cross_check(name: &str, source: &str) -> i32 {
    let dir = std::env::temp_dir();
    let gsrc = dir.join(format!("dcc-xcheck-{name}-{}.c", std::process::id()));
    let gexe = dir.join(format!("dcc-xcheck-{name}-{}.gcc", std::process::id()));
    fs::write(&gsrc, source).expect("write C fixture for gcc");
    let gbuild = Command::new("gcc")
        .args(["-O0", "-std=gnu2x", "-w", "-o"])
        .arg(&gexe)
        .arg(&gsrc)
        .output()
        .expect("run gcc");
    let (dcompile, dexe) = run_compile_and_run(name, source);
    let _ = fs::remove_file(&gsrc);
    assert!(
        gbuild.status.success(),
        "gcc 应能编译 {name}: {}",
        String::from_utf8_lossy(&gbuild.stderr)
    );
    assert!(
        dcompile.status.success(),
        "dcc 应能编译 {name}: {}",
        String::from_utf8_lossy(&dcompile.stderr)
    );
    let (gcode, gstdout, _gerr) = run_exe_bounded(&gexe);
    let (dcode, dstdout, _derr) = run_exe_bounded(&dexe);
    let _ = fs::remove_file(&gexe);
    let _ = fs::remove_file(&dexe);
    assert_eq!(
        String::from_utf8_lossy(&gstdout),
        String::from_utf8_lossy(&dstdout),
        "{name}: gcc 与 dcc 的 stdout 不一致 (gcc_rc={gcode} dcc_rc={dcode})"
    );
    assert_eq!(gcode, dcode, "{name}: gcc 与 dcc 的退出码不一致");
    dcode
}


/// 带优化档位的 gcc 交叉对照: 同一 C 源由 `gcc -O0` 与 `dcc <dcc_flags>` 各自编译,
/// 要求 stdout 逐字节一致且退出码相同, 返回该退出码供调用方再断言具体值。
///
/// 为什么必须新添一个而不是复用 [`run_gcc_cross_check`]: 后者内部走
/// `run_compile_and_run`, 零参数 = **-O0**。本项目全部“优化夹具”（
/// `tests/integration/opt.rs` 里的 m16_*）就是这样“用 -O0 的结果当 -O2 的结论”:
/// 同一批夹具手动加 `-O2` 后有 5 个 SEGV/HANG/错值, 而门禁全绿。
/// 优化类测试的参照系必须是 gcc, 否则两个编译器“一致地错”依旧通过。
///
/// 运行两侧均走 [`run_exe_bounded`]: -O2 的误编译典型形态是死循环, 无上限时会把
/// 整个测试目标吊死而不是跑红。
pub(crate) fn run_gcc_cross_check_opt(name: &str, source: &str, dcc_flags: &[&str]) -> i32 {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let gsrc = dir.join(format!("dcc-xopt-{name}-{pid}.c"));
    let gexe = dir.join(format!("dcc-xopt-{name}-{pid}.gcc"));
    fs::write(&gsrc, source).expect("write C fixture for gcc");
    let gbuild = Command::new("gcc")
        .args(["-O0", "-std=gnu2x", "-w", "-o"])
        .arg(&gexe)
        .arg(&gsrc)
        .output()
        .expect("run gcc");
    let (dbuild, dexe) = run_compile_and_run_flags(name, source, dcc_flags);
    let _ = fs::remove_file(&gsrc);
    assert!(
        gbuild.status.success(),
        "{name} [{}]: gcc 应能编译: {}",
        dcc_flags.join(" "),
        String::from_utf8_lossy(&gbuild.stderr)
    );
    assert!(
        dbuild.status.success(),
        "{name} [{}]: dcc 应能编译: {}",
        dcc_flags.join(" "),
        String::from_utf8_lossy(&dbuild.stderr)
    );
    let (gcode, gstdout, _) = run_exe_bounded(&gexe);
    let (dcode, dstdout, _) = run_exe_bounded(&dexe);
    let _ = fs::remove_file(&gexe);
    let _ = fs::remove_file(&dexe);
    assert_eq!(
        String::from_utf8_lossy(&gstdout),
        String::from_utf8_lossy(&dstdout),
        "{name} [{}]: gcc 与 dcc 的 stdout 不一致 (gcc_rc={gcode} dcc_rc={dcode})",
        dcc_flags.join(" ")
    );
    assert_eq!(
        gcode, dcode,
        "{name} [{}]: gcc 与 dcc 的退出码不一致",
        dcc_flags.join(" ")
    );
    dcode
}



/// 带环境变量与档位的汇编输出：给「某条 pass 是否真的改变了产物」这类断言用。
///
/// 为什么必须有它：一次 pass 接进管线后，单测全绿并不能证明它到达了产物
/// （docs/ssa-pass-upgrade-plan.md §7.5 就是这条教训 —— 旧 P16 的接入点放在管线
/// 末尾时，开与关的汇编逐字节相同）。唯一可信的证据是「关掉它，产物不一样」，
/// 而这需要一个能传 `DCC_OPT_SKIP` 的编译入口。
///
/// 返回 `(汇编文本, stderr)`：stderr 一并给出，是因为「诊断里说绑了谁」与「产物是否
/// 因此改变」是两个必须同时成立的证据，只看其中一个都会假绿。
pub(crate) fn compile_asm_text_with_env(
    name: &str,
    source: &str,
    flags: &[&str],
    envs: &[(&str, &str)],
) -> (String, String) {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-asmenv-{name}-{pid}.c"));
    let out = dir.join(format!("dcc-asmenv-{name}-{pid}.s"));
    fs::write(&src, source).expect("write C source");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-S").arg("-o").arg(&out);
    for f in flags {
        cmd.arg(f);
    }
    cmd.arg(&src);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let status = cmd.output().expect("run dcc");
    assert!(
        status.status.success(),
        "dcc 编译失败: {src:?}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let text = fs::read_to_string(&out).unwrap_or_default();
    let err = String::from_utf8_lossy(&status.stderr).into_owned();
    let _ = fs::remove_file(src);
    let _ = fs::remove_file(out);
    (text, err)
}

/// 跨 TU ABI 互操作对照: 同一对 C 源 (provider 无 main, consumer 有 main)
/// 用 gcc / dcc **各自单独编译**后由主机 gcc 链接, 要求四种组合的 stdout
/// 逐字节一致且退出码相同:
///  (gcc|gcc) 参照基准、(dcc|dcc) 全 dcc、(gcc|dcc) 与 (dcc|gcc) 混合。
///
/// 为什么必须是四种而不是两个单文件对照: `run_gcc_cross_check` 里 caller 与
/// callee 由同一个编译器产出, 两侧**对称地**错 (例如把 `_Complex double` 的两个
/// SSE eightbyte 都塞进 xmm0, 或把聚合的第二字读漏) 时程序内部依然自洽、
/// 退出码依然 0 —— 只有让一侧的对象表示由另一个实现负责, ABI 约定才成为
/// 被检验的对象。混合链接正是 `_Complex` / 聚合传参这类缺陷的唯一真判据。
#[cfg(unix)]
pub(crate) fn run_gcc_two_tu_cross_check(name: &str, provider: &str, consumer: &str) -> i32 {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let psrc = dir.join(format!("dcc-2tu-{name}-{pid}-p.c"));
    let csrc = dir.join(format!("dcc-2tu-{name}-{pid}-c.c"));
    fs::write(&psrc, provider).expect("write provider");
    fs::write(&csrc, consumer).expect("write consumer");

    // obj 前缀 "g" = gcc 产出, "d" = dcc 产出
    let dcc_bin = env!("CARGO_BIN_EXE_dcc");
    let mut objs: Vec<(char, std::path::PathBuf)> = Vec::new();
    for (compiler, tag, extra) in [
        ("gcc", 'g', vec!["-O0", "-std=gnu2x", "-w"]),
        (dcc_bin, 'd', vec!["-std=gnu23"]),
    ] {
        for (src, which) in [(&psrc, "p"), (&csrc, "c")] {
            let obj = dir.join(format!("dcc-2tu-{name}-{pid}-{tag}-{which}.o"));
            let mut cmd = Command::new(compiler);
            for f in &extra {
                cmd.arg(f);
            }
            let out = cmd
                .args(["-c", "-o"])
                .arg(&obj)
                .arg(src)
                .output()
                .unwrap_or_else(|e| panic!("{compiler} 编译 {name} 失败: {e}"));
            assert!(
                out.status.success(),
                "{compiler} 应能编译 {name} ({which}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
            objs.push((tag, obj));
        }
    }
    let g = |tag: char, which: &str| -> &std::path::Path {
        objs.iter()
            .find(|(t, p)| {
                *t == tag
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .ends_with(&format!("-{which}.o"))
            })
            .map(|(_, p)| p.as_path())
            .expect("object produced above")
    };

    let run = |combo: &[&std::path::Path]| -> (i32, String) {
        let exe = dir.join(format!("dcc-2tu-{name}-{pid}-link.bin"));
        let mut cmd = Command::new("gcc");
        cmd.arg("-w");
        for o in combo {
            cmd.arg(o);
        }
        cmd.arg("-o").arg(&exe);
        let link = cmd.output().expect("host gcc link");
        assert!(
            link.status.success(),
            "{name}: 链接 {:?} 失败: {}",
            combo
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&exe).output().expect("run linked exe");
        let _ = fs::remove_file(&exe);
        (
            run.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&run.stdout).into_owned(),
        )
    };

    // (gcc|gcc) 为参照, 其余三种都必须与它逐字节一致
    let want = run(&[g('g', "p"), g('g', "c")]);
    for (label, combo) in [
        ("dcc|dcc", vec![g('d', "p"), g('d', "c")]),
        ("gcc|dcc", vec![g('g', "p"), g('d', "c")]),
        ("dcc|gcc", vec![g('d', "p"), g('g', "c")]),
    ] {
        let got = run(&combo);
        assert_eq!(want.1, got.1, "{name}: {label} 组合的 stdout 与 gcc|gcc 参照不一致");
        assert_eq!(want.0, got.0, "{name}: {label} 组合的退出码与参照不一致");
    }
    for (_, obj) in &objs {
        let _ = fs::remove_file(obj);
    }
    let _ = fs::remove_file(&psrc);
    let _ = fs::remove_file(&csrc);
    want.0
}


/// C++ 夹具的 g++ 逐字节 + 退出码对照。实现**完全委派**
/// [`cpp_coroutine_cross_check`] (同一真源: g++ -std=c++20 参照、`dcc -c`、
/// 主机 gcc 链接、3 轮自洽), 只是换个与协程无关的名字 —— 块名/迭代序
/// 驱动的后端 (switch `case_<v>` 前缀匹配) 同样需要多轮自洽。
#[cfg(unix)]
pub(crate) fn cpp_gcc_cross_check(name: &str, source: &str) {
    cpp_coroutine_cross_check(name, source)
}


#[cfg(unix)]
pub(crate) fn run_sanitized_binary(
    name: &str,
    source: &str,
    flags: &[&str],
) -> (std::process::Output, std::process::Output, String, std::path::PathBuf) {
    let (compile, exe_path) = run_compile_and_run_flags(name, source, flags);
    assert!(
        compile.status.success(),
        "dcc should compile {name}: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&exe_path)
        .output()
        .unwrap_or_else(|e| panic!("run {name} binary: {e}"));
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    (compile, run, log, exe_path)
}


#[cfg(unix)]
pub(crate) fn assert_runtime_fails_with(run: &std::process::Output, log: &str, needles: &[&str]) {
    assert!(
        !run.status.success(),
        "sanitized binary should fail at runtime, rc={:?}, log={log}",
        run.status.code()
    );
    assert!(
        needles.iter().any(|needle| log.contains(needle)),
        "runtime log should contain one of {needles:?}: {log}"
    );
}


#[cfg(unix)]
pub(crate) fn assert_exit_code(run: &std::process::Output, code: i32, log: &str) {
    assert_eq!(
        run.status.code().unwrap_or(-1),
        code,
        "unexpected sanitizer handler exit code, log={log}"
    );
}


pub(crate) fn tool_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}


pub(crate) fn qemu_runtime_target(
    name: &str,
    target: &str,
    qemu: &str,
    expected: i32,
    source: &str,
) -> Option<()> {
    if !tool_exists(qemu) {
        eprintln!("skip {target} runtime: {qemu} not found");
        return None;
    }
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let exe_path = dir.join(format!("dcc-{name}-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["--target", target, "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "target {target}: compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(qemu)
        .arg(&exe_path)
        .output()
        .expect("run qemu");
    let _ = fs::remove_file(&exe_path);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        expected,
        "target {target}: runtime stderr={} stdout={}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
    Some(())
}


pub(crate) fn compile_legacy_asm_object(name: &str, source: &str) -> (Vec<u8>, String) {
    let (output, obj_path) = run_object_test(name, source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");

    let obj = fs::read(&obj_path).expect("read object file");
    let text = elf64_section_data(&obj, ".text")
        .expect("missing .text section")
        .to_vec();
    let dump = Command::new("objdump")
        .args(["-d"])
        .arg(&obj_path)
        .output()
        .expect("objdump");
    let _ = fs::remove_file(&obj_path);
    assert!(dump.status.success(), "objdump -d failed");
    let disasm = String::from_utf8_lossy(&dump.stdout).into_owned();
    (text, disasm)
}


pub(crate) fn assert_bytes_present(text: &[u8], needle: &[u8], label: &str) {
    assert!(
        text.windows(needle.len()).any(|w| w == needle),
        "missing {label} bytes {needle:02x?} in .text {text:02x?}"
    );
}


pub(crate) fn assert_disasm_present(disasm: &str, needle: &str) {
    assert!(disasm.contains(needle), "missing disassembly pattern `{needle}`:\n{disasm}");
}


/// C++ 前端集成测试: 编译 .cpp 到 .o (m17_* fixture 用)
/// 与 run_object_test 类似, 但显式写 .cpp 扩展名以触发 C++ 前端.
pub(crate) fn run_cpp_object_test(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cpp-{name}-{}.cpp", std::process::id()));
    let object_path = dir.join(format!("dcc-cpp-{name}-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write C++ fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    (output, object_path)
}


/// M51: 用主机 g++ 把 dcc 产出的 .o 链接成可执行文件并运行, 返回退出码.
/// 链接失败返回 -2, 主机没有 g++ 返回 -100 (便于和夹具自身的断言退出码区分).
#[allow(dead_code)]
pub(crate) fn host_link_cpp_object_and_run(name: &str, object: &std::path::Path) -> i32 {
    if Command::new("g++").arg("--version").output().is_err() {
        return -100;
    }
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-m51-link-{name}-{}.bin", std::process::id()));
    let link = Command::new("g++")
        .arg("-o")
        .arg(&exe)
        .arg(object)
        .output()
        .expect("run g++");
    if !link.status.success() {
        let _ = fs::remove_file(&exe);
        eprintln!("g++ link failed: {}", String::from_utf8_lossy(&link.stderr));
        return -2;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
    }
    let run = Command::new(&exe).output().expect("run exe");
    let _ = fs::remove_file(&exe);
    run.status.code().unwrap_or(-3)
}


/// M51 配套真值: 用主机 g++ **原生**编译并运行同一 C++ 夹具, 返回退出码.
/// 与 `host_link_cpp_object_and_run` 配对使用: 后者跑的是 dcc 产出的代码,
/// 前者是 g++ 自己的代码, 两者退出码必须相等. 主机无 g++ 返回 -100,
/// g++ 编译失败返回 -4 (与 dcc 侧的 -1/-2/-3 区分).
#[allow(dead_code)]
pub(crate) fn gxx_native_cpp_exit_code(name: &str, source: &str) -> i32 {
    if Command::new("g++").arg("--version").output().is_err() {
        return -100;
    }
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-gxx-native-{name}-{}.cpp", std::process::id()));
    let exe = dir.join(format!("dcc-gxx-native-{name}-{}.bin", std::process::id()));
    fs::write(&src, source).expect("write g++ fixture");
    let compile = Command::new("g++")
        .arg("-std=c++17")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("run g++");
    let _ = fs::remove_file(&src);
    if !compile.status.success() {
        eprintln!("g++ native compile failed: {}", String::from_utf8_lossy(&compile.stderr));
        return -4;
    }
    let run = Command::new(&exe).output().expect("run exe");
    let _ = fs::remove_file(&exe);
    run.status.code().unwrap_or(-3)
}


/// dcc -c + 主机 gcc 链接 + 运行. 用于验证运行时行为 (libc 调用等).
/// 返回 (编译输出, 链接+运行退出码). 退出码 0 表示通过.
/// link_extra_args: 传给 gcc 的额外参数 (如 "-lm" 用于 libm 转发).
pub(crate) fn run_host_link_and_run(name: &str, source: &str) -> (std::process::Output, i32) {
    run_host_link_and_run_with(name, source, &[])
}


pub(crate) fn run_host_link_and_run_with(
    name: &str,
    source: &str,
    link_extra_args: &[&str],
) -> (std::process::Output, i32) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-link-{name}-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-link-{name}-{}.o", std::process::id()));
    let exe_path = dir.join(format!("dcc-link-{name}-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    if !compile.status.success() {
        let _ = fs::remove_file(&object_path);
        return (compile, -1);
    }
    let link = Command::new("gcc")
        .args(["-o"])
        .arg(&exe_path)
        .arg(&object_path)
        .args(link_extra_args)
        .output()
        .expect("run gcc");
    let _ = fs::remove_file(&object_path);
    if !link.status.success() {
        let combined = std::process::Output {
            status: link.status,
            stdout: compile.stdout,
            stderr: {
                let mut s = compile.stderr;
                s.extend_from_slice(b"\n--- link ---\n");
                s.extend_from_slice(&link.stderr);
                s
            },
        };
        return (combined, -2);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
    }
    let run = Command::new(&exe_path).output().expect("run exe");
    let _ = fs::remove_file(&exe_path);
    let code = run.status.code().unwrap_or(-3);
    (compile, code)
}


/// 解析外部 GCC 源码树位置 (供 M7 libdecnumber / M26.6 cc1plus 两个集成用例共用).
///
/// 契约 (不留"静默绿"缺口):
///   - `GCC_SRC` 指向真实源码树且含 `required_subdir` → `Some(tree)`;
///   - 否则: 默认模式打印一行 SKIP 并返回 `None` (本机无 GCC 树时 `cargo test` 仍全绿,
///     与仓内其他依赖外部工具的用例同一写法);
///   - 打开 cargo feature `gcc_src` 即**严格模式**: 缺树直接失败, 因为该 feature 的
///     存在意义就是"我有源码树, 必须真跑", 不能让环境缺失伪装成通过.
#[cfg(unix)]
pub(crate) fn gcc_src_tree_or_skip(required_subdir: &str) -> Option<std::path::PathBuf> {
    let tree = std::env::var("GCC_SRC").ok().map(std::path::PathBuf::from);
    if let Some(p) = tree {
        if p.join(required_subdir).exists() {
            return Some(p);
        }
    }
    let reason = format!(
        "GCC source tree with `{required_subdir}/` not found (GCC_SRC={:?})",
        std::env::var("GCC_SRC").ok()
    );
    if cfg!(feature = "gcc_src") {
        panic!("`gcc_src` feature requires GCC_SRC to point at a real GCC source tree: {reason}");
    }
    eprintln!("SKIP: {reason}");
    None
}


/// 从汇编文本中提取 `funcname:` 到下一个 `.size funcname, .-funcname` 的内容
pub(crate) fn extract_function_body(asm: &str, name: &str) -> String {
    let label = format!("\n{name}:\n");
    let start = match asm.find(&label) {
        Some(s) => s + 1, // skip the leading '\n'
        None => return String::new(),
    };
    let end_marker = format!(".size {name}, .-{name}");
    let end = asm[start..]
        .find(&end_marker)
        .map(|i| start + i)
        .unwrap_or(asm.len());
    asm[start..end].to_string()
}


/// 通用工具：编译一个 C 源文件并返回 output + 可执行路径
#[allow(dead_code)]
pub(crate) fn run_compile_o(
    name: &str,
    source: &str,
    opt: &str,
) -> (std::process::Output, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let exe_path = dir.join(format!("dcc-{name}-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", opt, "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
    }
    (output, exe_path)
}


// ---------- kernel object corpus ----------

pub(crate) fn assert_kernel_corpus_object(rel: &str, expected_symbol: &str) {
    let path = fixtures_dir().join("corpus").join(rel);
    let obj = fs::read(&path).unwrap_or_else(|e| panic!("missing corpus object {path:?}: {e}"));
    assert!(obj.len() >= 64, "object too small: {path:?}");
    assert_eq!(&obj[0..4], &[0x7f, b'E', b'L', b'F'], "bad ELF magic: {path:?}");
    assert_eq!(obj[4], 2, "expected ELF64 class: {path:?}");
    assert_eq!(obj[5], 1, "expected little-endian ELF: {path:?}");
    let kind = u16::from_le_bytes([obj[16], obj[17]]);
    let machine = u16::from_le_bytes([obj[18], obj[19]]);
    assert_eq!(kind, 1, "expected ET_REL object: {path:?}");
    assert_eq!(machine, 0x3e, "expected EM_X86_64 object: {path:?}");

    let sections = elf64_sections(&obj);
    for section in [
        ".text",
        ".symtab",
        ".strtab",
        ".orc_unwind",
        ".orc_unwind_ip",
    ] {
        let (_, _, size) = sections
            .get(section)
            .unwrap_or_else(|| panic!("missing section {section} in {path:?}"));
        assert!(*size > 0, "empty section {section} in {path:?}");
    }

    let funcs = elf64_func_symbols(&obj);
    assert!(
        funcs
            .iter()
            .any(|(name, _, size)| name == expected_symbol && *size > 0),
        "missing expected function {expected_symbol} in {path:?}: {funcs:?}"
    );
}


/// M53: 模板 / 别名模板 / 协程-awaitor 夹具的统一交叉验证.
///
/// 与 `gxx_native_cpp_exit_code` 的区别: 那个固定用 `-std=c++17`, 而本域夹具都是
/// C++20 语法 (模板别名 / concepts / 协程). 流程:
///   1. `g++ -std=c++20` 必须**接受**夹具 (否则夹具只是 dcc 方言, 失去意义);
///   2. `g++ -std=c++20` 编译运行得参照退出码;
///   3. `dcc -c` + host g++ 链接 + 运行, 退出码必须与参照相等.
///
/// 夹具把每个验证点编成 `return N`, 所以退出码相等就是逐条语义一致.
/// 主机无 g++ 时降级为"dcc 侧必须 exit 0".
#[cfg(unix)]
pub(crate) fn cpp_std20_exit_code_cross_check(name: &str, source: &str) {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-cpp-{name}-{}.cpp", std::process::id()));
    fs::write(&src, source).expect("write fixture");
    let reference: Option<i32> = if Command::new("g++").arg("--version").output().is_ok() {
        let exe = dir.join(format!("dcc-cpp-{name}-{}-ref.bin", std::process::id()));
        let g = Command::new("g++")
            .args(["-std=c++20", "-o"])
            .arg(&exe)
            .arg(&src)
            .output()
            .expect("run g++");
        assert!(
            g.status.success(),
            "{name}: g++ -std=c++20 必须接受该夹具: {}",
            String::from_utf8_lossy(&g.stderr)
        );
        let run = Command::new(&exe)
            .output()
            .expect("run g++ reference binary");
        let _ = fs::remove_file(&exe);
        Some(run.status.code().unwrap_or(-3))
    } else {
        None
    };
    let _ = fs::remove_file(&src);

    let (out, obj) = run_cpp_object_test(name, source);
    assert!(
        out.status.success(),
        "{name}: dcc -c 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got = host_link_cpp_object_and_run(name, &obj);
    let _ = fs::remove_file(&obj);
    match reference {
        Some(want) => assert_eq!(
            got, want,
            "{name}: dcc 退出码 {got} 与 g++ -std=c++20 的 {want} 不一致 (夹具内 `return N` 逐条对应验证点)"
        ),
        None => assert_eq!(got, 0, "{name}: 无 g++ 参照, dcc 侧必须 exit 0 (实得 {got})"),
    }
}


// =========================================================================
// Section 11.4: x86_64 SysV va_arg 全栈支持
// =========================================================================

pub(crate) const VA_LIST_TYPEDEF: &str = "typedef __builtin_va_list va_list;\n";


// ====== M26: 协程真状态机 / modules BMF / GCC ABI 运行时 ======

/// C++20 协程三测试共用的交叉验证骨架.
///
/// 流程: 夹具先经 `g++ -std=c++20` 编译并运行作为**参照基准** (宿主无 g++ 则降级为
/// 只看 dcc 自己), 然后 `dcc -c` + host gcc 链接 + 运行, 比对退出码与 stdout.
///
/// 为什么还跑 **3 遍** dcc: 协程状态机是一个多臂 switch CFG, 一旦后端某处用
/// HashMap 迭代序驱动块名匹配 (`case_<v>` 前缀查找就是实测踩到的坑), 同一份源码
/// 每次进程会得到不同代码而退出码随机翻转. 只跑一遍能偶然对上 g++, 掩盖这类 bug.
pub(crate) fn cpp_coroutine_cross_check(name: &str, source: &str) {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-cpp-{name}-{}.cpp", std::process::id()));
    fs::write(&src_path, source).expect("write C++ coroutine fixture");

    let mut reference: Option<(i32, String)> = None;
    if Command::new("g++").arg("--version").output().is_ok() {
        let ref_bin = dir.join(format!("dcc-cpp-{name}-{}-ref.bin", std::process::id()));
        let g = Command::new("g++")
            .args(["-std=c++20", "-o"])
            .arg(&ref_bin)
            .arg(&src_path)
            .output()
            .expect("run g++");
        // 夹具必须是标准 C++20: g++ 拒收即夹具退化成方言子集, 直接失败.
        assert!(
            g.status.success(),
            "{name}: g++ -std=c++20 必须接受该夹具: {}",
            String::from_utf8_lossy(&g.stderr)
        );
        let grun = Command::new(&ref_bin)
            .output()
            .expect("run g++ reference binary");
        let _ = fs::remove_file(&ref_bin);
        reference = Some((
            grun.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&grun.stdout).to_string(),
        ));
    }

    let obj = dir.join(format!("dcc-cpp-{name}-{}.o", std::process::id()));
    let mut dcc_run: Option<(i32, String)> = None;
    for round in 0..3 {
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-o"])
            .arg(&obj)
            .arg(&src_path)
            .output()
            .expect("run dcc");
        assert!(
            out.status.success(),
            "{name}: dcc -c 失败 (round {round}): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let exe = dir.join(format!("dcc-cpp-{name}-{}.bin", std::process::id()));
        let link = Command::new("gcc")
            .arg(&obj)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("host gcc link");
        assert!(
            link.status.success(),
            "{name}: link 失败: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let run = Command::new(&exe).output().expect("run coroutine binary");
        let _ = fs::remove_file(&exe);
        let got = (
            run.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&run.stdout).to_string(),
        );
        if let Some(prev) = &dcc_run {
            assert_eq!(
                prev, &got,
                "{name}: dcc 自洽性破坏 (round {round} 与 round 0 结果不同, 通常是后端顺序依赖)"
            );
        }
        dcc_run = Some(got);
    }
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&src_path);
    let got = dcc_run.expect("dcc run result");
    match reference {
        Some(want) => {
            assert_eq!(
                want.0, got.0,
                "{name}: dcc 退出码 {} 与 g++ {} 不一致 (夹具内 `return N` 逐条对应验证点)",
                got.0, want.0
            );
            assert_eq!(want.1, got.1, "{name}: dcc stdout 与 g++ 不一致");
        },
        None => assert_eq!(got.0, 0, "{name}: 无 g++ 参照, dcc 侧必须 exit 0"),
    }
}


/// M28: 在 `dir` 里写 `<name>.cpp` 并用 dcc 编译为 `<name>.o`.
/// `emit_bmf` = true 时附 `--emit-module-file=<dir>/<name>.bmf`.
/// 返回子进程 Output (调用方自行断言 status / 读 stderr).
#[cfg(unix)]
pub(crate) fn dcc_compile_module(
    dir: &std::path::Path,
    name: &str,
    src: &str,
    emit_bmf: bool,
) -> std::process::Output {
    let sp = dir.join(format!("{}.cpp", name));
    fs::write(&sp, src).unwrap_or_else(|e| panic!("write {}: {}", sp.display(), e));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.args(["-c", "-o"])
        .arg(dir.join(format!("{}.o", name)))
        .arg(&sp);
    if emit_bmf {
        cmd.arg(format!("--emit-module-file={}", dir.join(format!("{}.bmf", name)).display()));
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("run dcc on {}: {}", name, e))
}


/// M28: 用 host gcc 链接若干 .o 并运行, 返回退出码 (链接失败 = -2).
#[cfg(unix)]
pub(crate) fn host_link_objects_and_run(exe: &std::path::Path, objs: &[&std::path::Path], cxx: bool) -> i32 {
    let linker = if cxx { "g++" } else { "gcc" };
    let mut cmd = Command::new(linker);
    for o in objs {
        cmd.arg(o);
    }
    cmd.arg("-o").arg(exe);
    let link = cmd
        .output()
        .unwrap_or_else(|e| panic!("run {}: {}", linker, e));
    if !link.status.success() {
        eprintln!("{} link failed: {}", linker, String::from_utf8_lossy(&link.stderr));
        return -2;
    }
    let run = Command::new(exe)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {}", exe.display(), e));
    run.status.code().unwrap_or(-1)
}


// =====================================================================
// Stage 0: C++ 三态路径 (`--cxx-native=off|on|auto`) 集成测试.
// =====================================================================
//
// 这些测试只覆盖 plain class / free function (Stage 3 端到端) 与
// throw/catch (Stage 9), 锁住 Stage 0 driver 调度的对外契约:
//
//   - `--cxx-native=off`   : 必须走 legacy 路径 (host cc)
//   - `--cxx-native=on`    : 必须走 native 路径 (dcc 内置 codegen)
//   - `--cxx-native=auto`  : 上述两类特性在 Auto 下应全走 native
//   - verbose 输出应报告每次调度的最终路径与 reason, 便于审计
//
// 这些测试假定机器有 host `cc` (gcc / clang). CI 无 host cc 时整段
// 跳过, 不视为失败.

#[cfg(unix)]
pub(crate) fn host_cc_available() -> bool {
    std::process::Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}


#[cfg(unix)]
pub(crate) fn run_cpp_with_mode(
    name: &str,
    source: &str,
    mode: &str,
) -> Option<(std::process::Output, std::path::PathBuf)> {
    if !host_cc_available() {
        eprintln!("skip {}: no host cc", name);
        return None;
    }
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cppmode-{name}-{}.cpp", std::process::id()));
    let object_path = dir.join(format!("dcc-cppmode-{name}-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write cpp fixture");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-c")
        .arg(format!("--cxx-native={}", mode))
        .arg("-v")
        .arg("-o")
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    if output.status.success() {
        Some((output, object_path))
    } else {
        let _ = fs::remove_file(&object_path);
        Some((output, object_path))
    }
}


#[cfg(unix)]
pub(crate) fn path_decision_in(stderr: &[u8]) -> &'static str {
    let s = String::from_utf8_lossy(stderr);
    for line in s.lines() {
        if line.contains("c++ backend decision") {
            if line.contains("path=legacy") {
                return "legacy";
            }
            if line.contains("path=native") {
                return "native";
            }
        }
    }
    "unknown"
}

// ---------------------------------------------------------------------------
// 以下助手由 `tests/integration/*.rs` 上移（§14：主题子文件里 `^fn` 数必须 ==
// `#[test]` 数 —— 被多条用例共用的助手不属于主题文件）。函数体逐字保持原语义，
// 只把可见性收敛成 `pub(crate)`、并把原先依赖壳 `use std::env` 的写法改成全路径。
// 每个助手的判据类别登记在 `scripts/gen_coverage_index.sh` 头部的推断表里。
// ---------------------------------------------------------------------------

/// 取某个函数的 asm 函数体（从 `<name>:` 到下一个行首标签或伪指令行）。
///
/// 两条 `seg_qualifier_*` 用例共用这一份：段前缀的断言必须**逐函数**做，全局计数会被
/// 别人的前缀凑数（把无关函数的前缀数进去就看不出某个形状真的没发）。保留 `seg_`
/// 前缀是因为主题子文件被壳以 `include!` 文本级注入同一个 crate，泛用名会互相撞车。
pub(crate) fn seg_asm_body(asm_text: &str, name: &str) -> String {
    let mut out = String::new();
    let mut hit = false;
    for line in asm_text.lines() {
        if line.trim_end() == format!("{}:", name) {
            hit = true;
            continue;
        }
        if !hit {
            continue;
        }
        // 下一个行首标签（助记符行都以空白开头）就是本函数结束。
        let is_label = !line.starts_with(' ')
            && !line.starts_with('\t')
            && !line.starts_with('.')
            && line.trim_end().ends_with(':');
        let is_section = !line.starts_with(' ') && !line.starts_with('\t') && line.starts_with('.');
        if is_label || is_section {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub(crate) fn seg_has_prefix(body: &str) -> bool {
    body.contains("%gs:") || body.contains("%fs:")
}

/// 把降级夹具编译一次, 返回 (stderr, 是否成功, .o 路径, .s 路径)。
///
/// 自 `tests/integration/c_asm.rs` 上移：7 条 `asm_fallback_*` 用例共用这一份编链动作，
/// 本身不跑产物 —— 判据面在调用方（看 stderr / 产物形态）。
pub(crate) fn asm_fallback_compile(
    tag: &str,
    extra: &[&str],
    out_ext: &str,
) -> (String, bool, std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let stem = format!("dcc-asm-fallback-{tag}-{}", std::process::id());
    let obj_path = dir.join(format!("{stem}{out_ext}"));
    let text_path = dir.join(format!("{stem}.s"));
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_fallback_gas.c");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-c").arg("-o").arg(&obj_path).args(extra).arg(&fixture);
    let output = cmd.output().expect("run dcc");
    (
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
        obj_path,
        text_path,
    )
}

/// 解析 ELF64 的 `SHT_RELA` 节（24 字节条目），返回 `(r_offset, 符号索引, 类型, 加数)`。
///
/// 为何不走 `readelf -s`/`-r` 的文本：本机 zh_CN.UTF-8 下 readelf 把 `STT_GNU_IFUNC`
/// 打成 `<操作系统专用>：10`（同一个词在 exe 里又打成 `IFUNC`），任何 `grep IFUNC` 的
/// 判据都会随 locale 假红/假绿。`ELF64_R_SYM` 是 r_info 的**高 32 位**、`ELF64_R_TYPE`
/// 是低 32 位（写反了会得到「每个 PC32 都指向 sym 2」这种看起来能自洽的错读数）。
/// （自 `tests/integration/c_attr_builtin.rs` 上移：`c_link_debug.rs` 也在用。）
pub(crate) fn elf64_rela(data: &[u8], section: &str) -> Vec<(u64, u32, u32, i64)> {
    let Some(raw) = elf64_section_data(data, section) else {
        return Vec::new();
    };
    raw.chunks_exact(24)
        .map(|e| {
            let info = u64::from_le_bytes(e[8..16].try_into().unwrap());
            (
                u64::from_le_bytes(e[0..8].try_into().unwrap()),
                (info >> 32) as u32,
                (info & 0xffff_ffff) as u32,
                i64::from_le_bytes(e[16..24].try_into().unwrap()),
            )
        })
        .collect()
}

/// 解析 ELF64 节头表，返回下标 = `st_shndx` 的节名向量（下标 0 是 SHT_NULL）。
///
/// 为何需要它：`__dcc_cpu_model` 必须坐在 `.data`（PROGBITS）而不是 `.bss`（NOBITS）——
/// 内置链接器的静态路径把 NOBITS 输出节钺错（`.bss` size=0、RW `PT_LOAD` 全 0，见
/// `codegen/x86_64/ifunc.rs::ensure_cpu_model` 的注释），坐 `.bss` 就写不进内存。
/// 只用 `elf64_sections`（名字→属性）拿不到「符号属于哪个节」，所以这里按节头序取名字。
pub(crate) fn elf64_section_names(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if data.len() < 64 {
        return out;
    }
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap()) as usize;
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().unwrap()) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().unwrap()) as usize;
    let e_shstrndx = u16::from_le_bytes(data[62..64].try_into().unwrap()) as usize;
    if e_shentsize < 64 || e_shoff + e_shentsize * e_shnum > data.len() {
        return out;
    }
    let shstr = e_shoff + e_shstrndx * e_shentsize;
    let shstr_off = u64::from_le_bytes(data[shstr + 24..shstr + 32].try_into().unwrap()) as usize;
    for i in 0..e_shnum {
        let off = e_shoff + i * e_shentsize;
        let name_off = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        out.push(elf64_read_cstr(&data[shstr_off.saturating_add(name_off)..]));
    }
    out
}

/// 按 `sh_type == SHT_SYMTAB(2)` 找符号表，返回 `(名字, bind, stype, shndx, value, size)`。
///
/// 为何需要它（而不是 `harness::elf64_syms`）：内置链接器的静态产物把 `.symtab`/`.strtab`/
/// `.shstrtab` 的节头 `sh_name` 都写成了 0（`src/link/elf_exec.rs` 里那五个 `write_shdr`
/// 调用第一个参数传 0，而 `lookup_sh_name` 只在用户节上用），所以 `readelf -SW` 里这些节
/// **没有名字**，按名字找的 `elf64_syms` 会返回空表。宿主 cc 产的 exe 不受影响。
/// 本助手只按节类型与 `sh_link` 取数，不依赖节名；它同时也是那个缺陷的现场证据。
///
/// `SHT_SYMTAB` 是 **2**，不是 3 —— `3` 是 `SHT_STRTAB`。取错会把字符串表当符号表读，
/// 得到满屏 ASCII 碎片（`val=0x2e00617461646f72` = `"rodata\0."`）这种看着像「解析器坏了」
/// 实则常量写错的红。符号条目大小优先信 `sh_entsize`，只在其为 0 时回落到 24。
pub(crate) fn elf64_syms_by_type(data: &[u8]) -> Vec<Elf64Sym> {
    const SHT_SYMTAB: u32 = 2;
    let mut out = Vec::new();
    if data.len() < 64 {
        return out;
    }
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap()) as usize;
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().unwrap()) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().unwrap()) as usize;
    if e_shentsize < 64 || e_shoff + e_shentsize * e_shnum > data.len() {
        return out;
    }
    // (sh_type, sh_offset, sh_size, sh_link, sh_entsize)
    let shdr = |i: usize| -> (u32, u64, u64, u32, u64) {
        let o = e_shoff + i * e_shentsize;
        (
            u32::from_le_bytes(data[o + 4..o + 8].try_into().unwrap()),
            u64::from_le_bytes(data[o + 24..o + 32].try_into().unwrap()),
            u64::from_le_bytes(data[o + 32..o + 40].try_into().unwrap()),
            u32::from_le_bytes(data[o + 40..o + 44].try_into().unwrap()),
            u64::from_le_bytes(data[o + 56..o + 64].try_into().unwrap()),
        )
    };
    let Some(sym_idx) = (0..e_shnum).find(|&i| shdr(i).0 == SHT_SYMTAB) else {
        return out;
    };
    let (_, sym_off, sym_size, strtab_idx, sym_ent) = shdr(sym_idx);
    let sym_ent = if sym_ent >= 24 { sym_ent as usize } else { 24 };
    if strtab_idx as usize >= e_shnum || (sym_off as usize) + (sym_size as usize) > data.len() {
        return out;
    }
    let (_, str_off, str_size, _, _) = shdr(strtab_idx as usize);
    if (str_off as usize) + (str_size as usize) > data.len() {
        return out;
    }
    let strtab = &data[str_off as usize..(str_off + str_size) as usize];
    for e in data[sym_off as usize..(sym_off + sym_size) as usize].chunks_exact(sym_ent) {
        let st_name = u32::from_le_bytes(e[0..4].try_into().unwrap()) as usize;
        out.push(Elf64Sym {
            name: if st_name > 0 && st_name < strtab.len() {
                elf64_read_cstr(&strtab[st_name..])
            } else {
                String::new()
            },
            bind: e[4] >> 4,
            stype: e[4] & 0xf,
            shndx: u16::from_le_bytes(e[6..8].try_into().unwrap()),
            value: u64::from_le_bytes(e[8..16].try_into().unwrap()),
            size: u64::from_le_bytes(e[16..24].try_into().unwrap()),
        });
    }
    out
}

/// 汇总一个产物里所有 `.rela*` 节的 RELA 条目。
///
/// 为什么不只看 `.rela.dyn`：irelative 放哪由 `ld` 根据输出类型決定——PIE/DYNAMIC 下
/// 在 `.rela.dyn`，非 PIE 的 ET_EXEC 下在 **`.rela.plt`**（实测：`dcc` 的 clones+printf
/// 产物 `readelf -rW` 里 `R_X86_64_IRELATIVE` 在 `.rela.plt`，gcc 的 PIE 产物在
/// `.rela.dyn`）。只扫 `.rela.dyn` 会在非 PIE 上假红，只扫 `.rela.plt` 则在 PIE 上假红。
pub(crate) fn elf64_all_rela(data: &[u8]) -> Vec<(u64, u32, u32, i64)> {
    let mut v = Vec::new();
    for sec in [".rela.dyn", ".rela.plt", ".rela.iplt"] {
        v.extend(elf64_rela(data, sec));
    }
    v
}

/// 走默认路径（dcc 出 `.o` → 宿主 `cc` 链接）编译源码，返回 `(成功?, stderr, exe 路径)`。
///
/// 不清理临时 exe：调用方要 `file`/`readelf`/运行它。PID 进文件名避免并行用例互踩，
/// 由调用方末尾删。（自 `tests/integration/c_attr_builtin.rs` 上移。）
pub(crate) fn dcc_link_exe(tag: &str, src: &str, flags: &[&str]) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sf = dir.join(format!("dcc-ifuncdyn-{tag}-{pid}.c"));
    let exe = dir.join(format!("dcc-ifuncdyn-{tag}-{pid}.bin"));
    fs::write(&sf, src).expect("写临时源文件");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-std=gnu23");
    for f in flags {
        cmd.arg(f);
    }
    cmd.args(["-o"]).arg(&exe).arg(&sf);
    let out = cmd.output().expect("运行 dcc");
    let _ = fs::remove_file(&sf);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        exe,
    )
}

/// 只编成 `.o` 并把它读回来（`.o` 侧的静态判据用）。
pub(crate) fn dcc_obj_bytes(tag: &str, src: &str, flags: &[&str]) -> Vec<u8> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sf = dir.join(format!("dcc-ifuncobj-{tag}-{pid}.c"));
    let obj = dir.join(format!("dcc-ifuncobj-{tag}-{pid}.o"));
    fs::write(&sf, src).expect("写临时源文件");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-std=gnu23").arg("-c");
    for f in flags {
        cmd.arg(f);
    }
    cmd.args(["-o"]).arg(&obj).arg(&sf);
    let out = cmd.output().expect("运行 dcc -c");
    assert!(out.status.success(), "compile [{}]: {}", flags.join(" "), String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(&sf);
    let bytes = fs::read(&obj).expect("读回 .o");
    let _ = fs::remove_file(&obj);
    bytes
}

/// 「这个产物是**动态**链接的」：`.interp`（PT_INTERP）+ `DT_NEEDED libc.so.6` +
/// `file` 报 dynamically linked，三项都得成立。
///
/// 三项而不是一项：`.interp` 只证明有解释器，`DT_NEEDED` 只证明有动态段，`file` 只是
/// 外部工具的读数 —— 「静默降级为静态」的缺陷形状是三者同时缺，判据也必须同时收。
pub(crate) fn assert_dynamically_linked(exe: &std::path::Path, who: &str, stderr: &str) {
    let bytes = fs::read(exe).unwrap_or_default();
    let secs = elf64_sections(&bytes);
    assert!(
        secs.contains_key(".interp"),
        "{who}: 产物没有 .interp（= 没有 PT_INTERP）= 静态可执行文件；IFUNC 降级缺陷回来了。stderr:\n{stderr}"
    );
    let need = elf64_dynamic_needed(&bytes);
    assert!(
        need.iter().any(|n| n == "libc.so.6"),
        "{who}: DT_NEEDED 里没有 libc.so.6（读到 {need:?}）；stderr:\n{stderr}"
    );
    let file_out = Command::new("file")
        .arg(exe)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    assert!(
        file_out.contains("dynamically linked"),
        "{who}: `file` 没报 dynamically linked:\n{file_out}"
    );
}

/// 把源码经指定 flags 链成可执行文件，返回 (是否成功, stderr 文本, 产物路径)。
/// 用 PID+stem 造唯一临时名，避免与并行用例互踩。（自 `tests/integration/c_link_debug.rs` 上移。）
#[cfg(unix)]
pub(crate) fn dcc_link_capture_flags(
    c_src: &str,
    stem: &str,
    flags: &[&str],
) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let tag = std::process::id();
    let sf = dir.join(format!("dcc-ifunc-snd-{stem}-{tag}.c"));
    let exe = dir.join(format!("dcc-ifunc-snd-{stem}-{tag}.bin"));
    std::fs::write(&sf, c_src).expect("写临时源文件");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-std=gnu23");
    for f in flags {
        cmd.arg(f);
    }
    cmd.args(["-o"]).arg(&exe).arg(&sf);
    let out = cmd.output().expect("运行 dcc");
    let _ = std::fs::remove_file(&sf);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), stderr, exe)
}

/// 逐档与 gcc 对照同一份夹具; 失效形态含 HANG, 故必须走有界运行。
/// （自 `tests/integration/opt_ssa_gate.rs` 上移：`c_lang.rs` 也在用。）
pub(crate) fn assert_all_levels_match_gcc(tag: &str, src: &str, why: &str) {
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

/// 逐字节对照汇编前摘掉 `.file` 行。
///
/// 为什么必须摘：`compile_asm_text_with_env` 把调用方给的 tag 写进源文件名，于是两次
/// 调用的产物**永远**在 `.file` 这一行上差一串字节。不摘的话 `assert_ne!` 会因为文件
/// 名而空跑（看起来绿，其实证明不了任何事），`assert_eq!` 会因为同一个原因无条件失败。
/// 实测这条不是理论问题：本轮新加的反向断言首跑就是被 `.file` 挡红的。
/// （自 `tests/integration/opt_ssa_gate.rs` 上移；只做文本归一，不贡献判据。）
pub(crate) fn asm_body(asm: &str) -> String {
    asm.lines()
        .filter(|l| !l.starts_with("\t.file"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 同一份源码、同一档位，只让 P16 开/关（`DCC_SSA_OFF=pipe`），返回 -O2 汇编。
/// （自 `tests/integration/opt_ssa_gate.rs` 上移；原先依赖壳的 `use std::env`，这里写全路径。）
pub(crate) fn dcc_asm_with_pipe(name: &str, src: &str, ssa_off: Option<&str>) -> String {
    let dir = std::env::temp_dir();
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



// ============================================================================
// Phase 6.2 收尾：gdb 实读 dcc `-g` 产物的共享助手（自 tests/integration/dwarf_gdb.rs 搬入）
//
// 搬进来的理由不是「凑过 STRUCT-HELPER」，而是 §14 的意图：主题子文件应当是一条
// `#[test]` 序列，共享的「怎么编、怎么跑 gdb、怎么读 readelf」属于这里。
// ============================================================================

/// 故意让「源码序」与「地址序」不同序：`sink` 在**第 1 行**就有原型，而它在 `.text` 里排在
/// `f` 之后。这正是 CU 区间取 `functions.first()/last()` 时会算错、取 min/max 时才对的形状
/// （见 `src/debug.rs::normalize_cu`）。`v`/`t` 两个局部各自走寄存器家与栈槽家。
pub(crate) const DWARF_GDB_SRC: &str = r#"int sink(int);
int f(void) {
    register int v asm("rbx") = 42;
    int t = sink(0);
    return v + t;
}
int sink(int x) { return x + 1; }
int main(void) { return f() - 43; }
"#;

/// 一个 DIE 的若干事实。为什么不自己写 DWARF 解析器：`.debug_abbrev` 是流式的，在本仓库里
/// 再放一份「谁知道属性怎么排」的实现就是第二知道；readelf 已是 DWARF 文本层的既有工具。
#[derive(Debug, Clone, Default)]
pub(crate) struct DwarfDieRow {
    pub(crate) tag: String,
    pub(crate) name: String,
    pub(crate) low: Option<u64>,
    pub(crate) high: Option<u64>,
    pub(crate) decl_line: Option<u64>,
    pub(crate) frame_base: bool,
    /// `DW_AT_location` 那行的原文（readelf 已把字节块反汇编成 `DW_OP_reg12 (r12)` 这种形）。
    /// 不自己解字节块：仓库里再放一份「谁知道位置表达式怎么排」就是第二知道。
    pub(crate) location: String,
}

/// gdb / readelf 是否可用。不可用时调用方跳过而不是失败（与 `host_cc_available` 同策略）。
pub(crate) fn dwarf_gdb_unavailable() -> Option<&'static str> {
    if !tool_exists("gdb") {
        return Some("gdb not found");
    }
    if !host_cc_available() {
        return Some("no host cc");
    }
    None
}

/// 把 `DWARF_GDB_SRC` 落成临时 `.c` 并按给定档位编译/链接。**故意不删源文件**：gdb 打印
/// 源码行、以及行断点按文件名解析都要用它。返回 (源路径, 产物路径)。
pub(crate) fn dwarf_gdb_build(
    tag: &str,
    flags: &[&str],
    link: bool,
) -> (std::path::PathBuf, std::path::PathBuf) {
    dwarf_gdb_build_env(tag, DWARF_GDB_SRC, flags, link, &[])
}

/// `dwarf_gdb_build` 的带环境变量版本：`-O2` 那条用例需要同时拿到**并行**与**串行**
/// 两份产物（`DCC_NO_PARALLEL=1` 是 `cli.rs` 里 `opt >= 2` 那条开关的唯一逃生门），
/// 而两者只能由环境区别。
pub(crate) fn dwarf_gdb_build_env(
    tag: &str,
    src: &str,
    flags: &[&str],
    link: bool,
    envs: &[(&str, &str)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sf = dir.join(format!("dcc-{tag}-{pid}.c"));
    let out = dir.join(format!("dcc-{tag}-{pid}.{}", if link { "bin" } else { "o" }));
    std::fs::write(&sf, src).expect("写临时 C 源");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-std=gnu23").args(flags);
    if !link {
        cmd.arg("-c");
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.arg("-o").arg(&out).arg(&sf);
    let res = cmd.output().expect("运行 dcc");
    assert!(
        res.status.success(),
        "dcc 编译 [{}] 失败：{}",
        flags.join(" "),
        String::from_utf8_lossy(&res.stderr)
    );
    (sf, out)
}

/// 跑一次 `gdb -batch` 会话，返回 stdout+stderr 合流（gdb 的诊断走 stderr，警告走 stdout）。
pub(crate) fn gdb_batch(exe: &std::path::Path, cmds: &[&str]) -> String {
    let mut cmd = std::process::Command::new("gdb");
    cmd.arg("-batch")
        .arg("-nx")
        .arg("-iex")
        .arg("set debuginfod enabled off")
        .arg("-ex")
        .arg("set pagination off");
    for c in cmds {
        cmd.arg("-ex").arg(c);
    }
    // readelf/gdb 在本机 zh_CN.UTF-8 下会翻译固定文案，判据靠英文字面串，必须锁 C locale。
    cmd.env("LC_ALL", "C").arg(exe);
    let out = cmd.output().expect("运行 gdb");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// 从 gdb 会话输出里取 `Breakpoint 1 at 0x....` 的地址（用于「三行绑到不同地址」这条判据）。
/// 取不到就点名是哪一行取的 —— 空串互不相等的假象会让判据假绿，所以这里直接 pan。
pub(crate) fn gdb_bp_addr(out: &str, which: &str) -> String {
    let p = out.find("Breakpoint 1 at 0x").unwrap_or_else(|| {
        panic!("{which} 那次会话里 gdb 没有报出 `Breakpoint 1 at 0x...`：\n{out}")
    });
    let rest = &out[p + "Breakpoint 1 at 0x".len()..];
    rest.split(|c: char| !matches!(c, '0'..='9' | 'a'..='f' | 'A'..='F'))
        .next()
        .unwrap_or("")
        .to_string()
}

/// 取 `DW_AT_<attr> : <value>` 一行的两段。
pub(crate) fn readelf_attr(line: &str) -> Option<(&str, &str)> {
    let idx = line.find("DW_AT_")?;
    let rest = &line[idx + 6..];
    let colon = rest.find(": ")?;
    Some((rest[..colon].trim(), rest[colon + 2..].trim()))
}

/// readelf 打的整数值：`0x401110`、`0x12d`，以及未重定位的 `.o` 里的裸 `0`。
pub(crate) fn readelf_int(v: &str) -> Option<u64> {
    let t = v.trim();
    let body = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(body, if body == t { 10 } else { 16 }).ok()
}

/// `readelf --debug-dump=info` 的文本。
pub(crate) fn readelf_debug_info_dump(path: &std::path::Path) -> String {
    let out = std::process::Command::new("readelf")
        .env("LC_ALL", "C")
        .arg("--debug-dump=info")
        .arg(path)
        .output()
        .expect("运行 readelf");
    assert!(
        out.status.success(),
        "readelf 失败：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// 把 `readelf --debug-dump=info` 的文本切成 DIE 行（以 `Abbrev Number: N (DW_TAG_...)` 分段）。
pub(crate) fn parse_debug_info(dump: &str) -> Vec<DwarfDieRow> {
    let mut rows: Vec<DwarfDieRow> = Vec::new();
    let mut cur: Option<DwarfDieRow> = None;
    for line in dump.lines() {
        if let Some(p) = line.find("(DW_TAG_") {
            if let Some(prev) = cur.take() {
                rows.push(prev);
            }
            let tail = &line[p + 1..];
            let tag = tail.split(')').next().unwrap_or("").to_string();
            cur = Some(DwarfDieRow { tag, ..Default::default() });
            continue;
        }
        let Some(row) = cur.as_mut() else { continue };
        let Some((attr, value)) = readelf_attr(line) else { continue };
        match attr {
            "name" => {
                row.name = value.rsplit(": ").next().unwrap_or(value).to_string();
            }
            "low_pc" => row.low = readelf_int(value),
            "high_pc" => row.high = readelf_int(value),
            "decl_line" => row.decl_line = readelf_int(value),
            "frame_base" => row.frame_base = true,
            "location" => row.location = value.to_string(),
            _ => {}
        }
    }
    if let Some(prev) = cur.take() {
        rows.push(prev);
    }
    rows
}

/// 取某个 `DW_TAG_variable` 的 `DW_AT_location` 原文。名字而不是地址是键：`-O2` 下
/// 谁绑上寄存器是分配器的事，用例只问「这个变量的位置表达式长什么样」。
pub(crate) fn var_location(rows: &[DwarfDieRow], name: &str) -> Option<String> {
    rows.iter()
        .find(|r| r.tag == "DW_TAG_variable" && r.name == name)
        .map(|r| r.location.clone())
        .filter(|s| !s.is_empty())
}

/// 从 `1 byte block: 5c \t(DW_OP_reg12 (r12))` 里取寄存器名（gdb 那边要拿它写
/// `set $r12 = ...`）。取不到就返 `None`，由调用方点名是哪个变量。
pub(crate) fn dwarf_reg_name(loc: &str) -> Option<String> {
    let open = loc.rfind('(')?;
    let close = loc[open..].find(')').map(|i| open + i)?;
    let inner = loc[open + 1..close].trim();
    let last = inner.rsplit([' ', '\t']).next()?;
    if last.starts_with('r') {
        Some(last.to_string())
    } else {
        None
    }
}
