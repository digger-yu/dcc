//! Stage 4/5/6/7 C++ Itanium ABI 与 GCC 的交叉验证。
//!
//! 验证的是**对象布局、生命周期与派发语义**，不是「能不能编过」：
//! - 多继承 secondary vtable 槽必须指向 this-adjust thunk
//! - 虚继承的虚基子对象要有自己的 vptr（chain vftable），覆写槽要带 thunk
//! - 虚基子对象只有一份共享实例，且被抬到最派生对象尾部
//! - 类对象尺寸（含 vptr 槽与尾部填充）要与 g++ 逐字节一致
//! - ctor-initializer / 隐式构造析构 / cv 重载的成员初始化顺序与实参个数
//!
//! 夹具自己把观察值 printf 出来，因此 **stdout 逐行相等**就是逐条一致；
//! 只看退出码会把「算错了但没触发断言」放过去。
//!
//! 同一套流程也有独立脚本 `scripts/cpp_abi_gcc_cross_check.sh`（可单独跑、
//! 给 CI 出报表）；这里把它接进 `cargo test`，让验收标准「全部测试用例通过」
//! 天然包含 GCC 对照。夹具按 `tests/fixtures/cpp_abi_<主题>/` 分组，
//! 新增分组目录会被 `cpp_abi_groups_are_all_covered` 抓到，不需要改这里的清单。
//!
//! 主机没有 g++ 时整个测试跳过（打印说明后 return），不误报失败。

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 一条夹具的运行结果：退出码 + stdout。
#[derive(Debug, PartialEq, Eq)]
struct RunResult {
    code: i32,
    stdout: String,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcc-cpp-abi-{name}-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir
}

fn gxx_available() -> bool {
    Command::new("g++").arg("--version").output().is_ok()
}

/// 列出某个夹具分组目录里的全部夹具 (按文件名排序, 结果稳定)。
fn fixtures_in(group: &str) -> Vec<PathBuf> {
    let dir = fixture_root().join(group);
    let mut out: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "cpp").unwrap_or(false))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// `tests/fixtures/` 下全部 `cpp_abi_*` 分组目录名。
fn all_groups() -> Vec<String> {
    let mut groups: Vec<String> = fs::read_dir(fixture_root())
        .expect("tests/fixtures 必须存在")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("cpp_abi_"))
        .collect();
    groups.sort();
    groups
}

/// 用 g++ 原生编译并运行夹具，得到真值。
fn gxx_truth(fixture: &Path, dir: &Path, std: &str) -> RunResult {
    let exe = dir.join("gxx.bin");
    let build = Command::new("g++")
        .arg(format!("-std={std}"))
        .arg("-O0")
        .arg("-no-pie")
        .arg("-o")
        .arg(&exe)
        .arg(fixture)
        .output()
        .expect("run g++");
    assert!(
        build.status.success(),
        "g++ 必须接受该夹具 (否则它只是 dcc 方言, 失去交叉验证意义): {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&exe).output().expect("run g++ binary");
    RunResult {
        code: run.status.code().unwrap_or(-3),
        stdout: String::from_utf8_lossy(&run.stdout).to_string(),
    }
}

/// 用 dcc 编译出 .o, 再由宿主链接器链 libc++/libstdc++ 运行。
fn dcc_result(fixture: &Path, dir: &Path) -> RunResult {
    let obj = dir.join("dcc.o");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-c")
        .arg("-o")
        .arg(&obj)
        .arg(fixture)
        .output()
        .expect("run dcc");
    assert!(
        compile.status.success(),
        "dcc -c 失败: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let exe = dir.join("dcc.bin");
    let link = Command::new("g++")
        .arg("-no-pie")
        .arg("-o")
        .arg(&exe)
        .arg(&obj)
        .output()
        .expect("run g++ link");
    assert!(
        link.status.success(),
        "链接 dcc 产物失败: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&exe).output().expect("run dcc binary");
    RunResult {
        code: run.status.code().unwrap_or(-3),
        stdout: String::from_utf8_lossy(&run.stdout).to_string(),
    }
}

/// 对单个夹具跑一遍 GCC 对照; 返回不一致的描述, `None` = 一致。
fn check_one(fixture: &Path) -> Option<String> {
    let name = fixture.file_stem()?.to_string_lossy().into_owned();
    let dir = work_dir(&name);
    let want = gxx_truth(fixture, &dir, "c++17");
    let got = dcc_result(fixture, &dir);
    let _ = fs::remove_dir_all(&dir);
    if want.code != got.code {
        return Some(format!("{name}: 退出码 gcc={} dcc={}", want.code, got.code));
    }
    if want.stdout != got.stdout {
        return Some(format!(
            "{name}: stdout 与 g++ 不一致\n--- gcc ---\n{}\n--- dcc ---\n{}",
            want.stdout, got.stdout
        ));
    }
    None
}

/// 跑一个分组; 分组内所有夹具都要一致, 失败一次性汇总报出。
fn check_group(group: &str) {
    if !gxx_available() {
        eprintln!("{group}: 主机无 g++, 跳过 GCC 交叉验证");
        return;
    }
    let fixtures = fixtures_in(group);
    assert!(!fixtures.is_empty(), "分组 {group} 里没有夹具");
    let mut failures: Vec<String> = Vec::new();
    for f in &fixtures {
        if let Some(msg) = check_one(f) {
            failures.push(msg);
        }
    }
    assert!(
        failures.is_empty(),
        "{} 个夹具与 g++ 不一致:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn cpp_abi_virtual_group() {
    check_group("cpp_abi_virtual");
}

/// Stage 4: 构造/析构顺序、委托构造、copy/move 构造、隐式构造析构。
#[test]
fn cpp_abi_lifecycle_group() {
    check_group("cpp_abi_lifecycle");
}

/// Stage 2/4: 重载 (参数类型、const 限定) 的链接名与候选选定。
#[test]
fn cpp_abi_overload_group() {
    check_group("cpp_abi_overload");
}

/// Stage 7: RTTI 与 dynamic_cast 的 native 端到端闭环。
#[test]
fn cpp_abi_rtti_group() {
    check_group("cpp_abi_rtti");
}

/// Stage 9: zero-cost EH (invoke/landing pad/LSDA) 的抛出-栈展开-捕获闭环。
///
/// 这一组的价值在于「运行时真的走通了表驱动展开」: 只要 LSDA 任一字段错位、
/// call-site 区间缺失或 FDE 描述不对, 结果就是 `std::terminate` (退出码 134)
/// 或析构/捕获顺序错乱, 与 g++ 的 stdout 对比会直接暴露。
#[test]
fn cpp_abi_eh_group() {
    check_group("cpp_abi_eh");
}

/// Stage 8: 模板实例的编译期常量与依赖名在表达式里的折叠。
///
/// 类型特征 (`is_pointer<T*>::value` / `is_same<T,U>::value`) 是 enable_if/偏特化
/// 匹配的地基: 折不出常量, 偏特化就选不中, 表现为「同一个程序里 traits 永远为 0」。
#[test]
fn cpp_abi_template_group() {
    check_group("cpp_abi_template");
}

/// 新增分组目录必须同时出现在上面的分组测试清单里, 否则交叉验证会漏跑。
#[test]
fn cpp_abi_groups_are_all_covered() {
    const COVERED: &[&str] = &[
        "cpp_abi_virtual",
        "cpp_abi_lifecycle",
        "cpp_abi_overload",
        "cpp_abi_rtti",
        "cpp_abi_eh",
        "cpp_abi_template",
    ];
    let missing: Vec<String> = all_groups()
        .into_iter()
        .filter(|g| !COVERED.contains(&g.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "cpp_abi 夹具分组 {missing:?} 还没有对应的交叉验证测试 (见本文件的分组测试)"
    );
}

/// 每条夹具都必须是 g++ 接受的真 C++ (不是 dcc 方言)。
#[test]
fn cpp_abi_fixtures_are_standard_cxx() {
    if !gxx_available() {
        eprintln!("主机无 g++, 跳过夹具语法校验");
        return;
    }
    for group in all_groups() {
        for f in fixtures_in(&group) {
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            let dir = work_dir(&format!("syntax-{stem}"));
            let _ = gxx_truth(&f, &dir, "c++17");
            let _ = fs::remove_dir_all(&dir);
        }
    }
}
