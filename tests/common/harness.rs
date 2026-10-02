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


