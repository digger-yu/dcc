//! GCC C Torture Suite integration tests.
//!
//! 运行方式:
//!   1. `bash scripts/setup_torture.sh` 下载测试集到 `third_party/gcc-torture/`
//!   2. `cargo test --release torture` 执行测试
//!
//! 测试类别:
//!   - `execute`:    编译 + 运行，验证退出码 == 0
//!   - `compile`:    仅编译，验证编译成功
//!
//! 跳过列表: `tests/torture_skip.txt`，每行一个文件名或 glob 模式。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// 返回 torture suite 根目录。优先读 TORTURE_DIR 环境变量。
fn torture_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TORTURE_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("third_party/gcc-torture")
}

fn suite_subdir(sub: &str) -> PathBuf {
    torture_dir()
        .join("gcc/testsuite/gcc.c-torture")
        .join(sub)
}

// ── Skip list ──────────────────────────────────────────────────────

fn load_skip_patterns() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/torture_skip.txt");
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn should_skip(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| glob_match(p, name))
}

/// 简易 glob 匹配: 仅支持 `*` 通配符。
fn glob_match(pattern: &str, text: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        text.starts_with(prefix)
    } else {
        pattern == text
    }
}

// ── 测试入口 ───────────────────────────────────────────────────────

#[test]
#[cfg(unix)]
fn torture_execute() {
    let dir = suite_subdir("execute");
    if !dir.exists() {
        eprintln!(
            "Torture suite 未安装。运行: bash scripts/setup_torture.sh"
        );
        return;
    }
    let tests = collect_c_files(&dir);
    assert!(
        !tests.is_empty(),
        "execute/ 目录为空，请检查 torture suite 安装"
    );
    let skip = load_skip_patterns();
    let (pass, fail, skip_count, failures) = run_execute_tests(&tests, &skip);
    report("execute", pass, fail, skip_count, &failures);
    assert!(
        failures.is_empty(),
        "{}/{} execute tests failed (skip={})",
        failures.len(),
        pass + fail,
        skip_count
    );
}

#[test]
#[cfg(unix)]
fn torture_compile() {
    let dir = suite_subdir("compile");
    if !dir.exists() {
        eprintln!(
            "Torture suite 未安装。运行: bash scripts/setup_torture.sh"
        );
        return;
    }
    let tests = collect_c_files(&dir);
    assert!(
        !tests.is_empty(),
        "compile/ 目录为空，请检查 torture suite 安装"
    );
    let skip = load_skip_patterns();
    let (pass, fail, skip_count, failures) = run_compile_tests(&tests, &skip);
    report("compile", pass, fail, skip_count, &failures);
    assert!(
        failures.is_empty(),
        "{}/{} compile tests failed (skip={})",
        failures.len(),
        pass + fail,
        skip_count
    );
}

// ── 文件收集 ───────────────────────────────────────────────────────

fn collect_c_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(|e| {
            let e = e.ok()?;
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "c") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    files
}

// ── execute 测试 ───────────────────────────────────────────────────

fn run_execute_tests(
    tests: &[PathBuf],
    skip: &[String],
) -> (usize, usize, usize, Vec<(String, String)>) {
    let pass = Mutex::new(0usize);
    let fail = Mutex::new(0usize);
    let skipped = Mutex::new(0usize);
    let failures = Mutex::new(Vec::<(String, String)>::new());

    // 使用与 cargo test 相同的线程数
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let chunks: Vec<Vec<&PathBuf>> = {
        let mut c: Vec<Vec<&PathBuf>> = vec![Vec::new(); threads];
        for (i, t) in tests.iter().enumerate() {
            c[i % threads].push(t);
        }
        c
    };

    std::thread::scope(|s| {
        for chunk in &chunks {
            let pass = &pass;
            let fail = &fail;
            let skipped = &skipped;
            let failures = &failures;
            s.spawn(move || {
                for path in chunk {
                    let name = path.file_name().unwrap().to_str().unwrap();
                    if should_skip(name, skip) {
                        *skipped.lock().unwrap() += 1;
                        continue;
                    }
                    match compile_and_run(path) {
                        TestResult::Pass => *pass.lock().unwrap() += 1,
                        TestResult::Fail(msg) => {
                            *fail.lock().unwrap() += 1;
                            failures.lock().unwrap().push((name.into(), msg));
                        }
                    }
                }
            });
        }
    });

    (
        pass.into_inner().unwrap(),
        fail.into_inner().unwrap(),
        skipped.into_inner().unwrap(),
        failures.into_inner().unwrap(),
    )
}

fn compile_and_run(path: &Path) -> TestResult {
    let tmp = std::env::temp_dir().join(format!(
        "dcc-torture-{}-{}",
        path.file_stem().unwrap().to_str().unwrap(),
        std::process::id()
    ));
    let dcc = env!("CARGO_BIN_EXE_dcc");

    // 编译
    let compile = Command::new(dcc)
        .args(["-std=gnu11", "-o"])
        .arg(&tmp)
        .arg(path)
        .output();

    let compile = match compile {
        Ok(o) => o,
        Err(e) => return TestResult::Fail(format!("spawn dcc: {e}")),
    };
    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        // 截断过长输出，保留关键错误信息
        let snippet = truncate(&stderr, 500);
        return TestResult::Fail(format!("compile failed: {snippet}"));
    }

    // 运行
    let run = Command::new(&tmp).output();
    let _ = fs::remove_file(&tmp);

    let run = match run {
        Ok(o) => o,
        Err(e) => return TestResult::Fail(format!("run: {e}")),
    };
    let code = run.status.code().unwrap_or(-1);
    if code != 0 {
        return TestResult::Fail(format!("exit={code}"));
    }
    TestResult::Pass
}

// ── compile 测试 ───────────────────────────────────────────────────

fn run_compile_tests(
    tests: &[PathBuf],
    skip: &[String],
) -> (usize, usize, usize, Vec<(String, String)>) {
    let pass = Mutex::new(0usize);
    let fail = Mutex::new(0usize);
    let skipped = Mutex::new(0usize);
    let failures = Mutex::new(Vec::<(String, String)>::new());

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let chunks: Vec<Vec<&PathBuf>> = {
        let mut c: Vec<Vec<&PathBuf>> = vec![Vec::new(); threads];
        for (i, t) in tests.iter().enumerate() {
            c[i % threads].push(t);
        }
        c
    };

    std::thread::scope(|s| {
        for chunk in &chunks {
            let pass = &pass;
            let fail = &fail;
            let skipped = &skipped;
            let failures = &failures;
            s.spawn(move || {
                for path in chunk {
                    let name = path.file_name().unwrap().to_str().unwrap();
                    if should_skip(name, skip) {
                        *skipped.lock().unwrap() += 1;
                        continue;
                    }
                    match compile_only(path) {
                        TestResult::Pass => *pass.lock().unwrap() += 1,
                        TestResult::Fail(msg) => {
                            *fail.lock().unwrap() += 1;
                            failures.lock().unwrap().push((name.into(), msg));
                        }
                    }
                }
            });
        }
    });

    (
        pass.into_inner().unwrap(),
        fail.into_inner().unwrap(),
        skipped.into_inner().unwrap(),
        failures.into_inner().unwrap(),
    )
}

fn compile_only(path: &Path) -> TestResult {
    let tmp = std::env::temp_dir().join(format!(
        "dcc-torture-c-{}-{}.o",
        path.file_stem().unwrap().to_str().unwrap(),
        std::process::id()
    ));
    let dcc = env!("CARGO_BIN_EXE_dcc");

    let out = Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&tmp)
        .arg(path)
        .output();

    let out = match out {
        Ok(o) => o,
        Err(e) => return TestResult::Fail(format!("spawn dcc: {e}")),
    };
    let _ = fs::remove_file(&tmp);

    if out.status.success() {
        TestResult::Pass
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        TestResult::Fail(format!("compile failed: {}", truncate(&stderr, 500)))
    }
}

// ── 工具函数 ───────────────────────────────────────────────────────

enum TestResult {
    Pass,
    Fail(String),
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}... [truncated]", &s[..max])
    }
}

fn report(category: &str, pass: usize, fail: usize, skip: usize, failures: &[(String, String)]) {
    let total = pass + fail + skip;
    eprintln!();
    eprintln!("=== Torture [{category}] ===");
    eprintln!(
        "  total={total}  pass={pass}  fail={fail}  skip={skip}"
    );
    if !failures.is_empty() {
        eprintln!("  Failures:");
        for (name, msg) in failures {
            eprintln!("    FAIL  {name}: {msg}");
        }
    }
}
