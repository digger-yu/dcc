//! Stage 3b 验收: `--cxx-native=auto|off|on` 的 **CLI 落地契约**。
//!
//! 方案 §5.1 的三条硬要求都只能在真实命令行上验证:
//! - `off` 强制 legacy, 且 verbose 里能看到决策与原因;
//! - `on` **不得静默 fallback** (native 覆盖率要能被直接量化);
//! - `auto` 的 fallback **必须在 verbose 输出中可见** (含原因), 并进路径统计。
//!
//! 另外 `--cxx-native` 必须出现在 `--help` 里: 用户与脚本选不出这个开关,
//! 就等于它没落地。非法取值也必须是硬失败 (非 0 退出), 不能默默退回默认值。

use std::process::Command;

fn dcc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dcc"))
}

/// 写一个临时源文件, 返回其路径。
fn write_src(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).expect("写入临时源文件");
    p
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dcc-cxx-native-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

const PLAIN_CLASS: &str = r#"
struct P { int x; int get() const { return x; } };
int main() { P p; p.x = 3; return p.get(); }
"#;

/// native 的 lowering 会拒绝、而能力矩阵不会预先拦下的写法 (闭包)。
/// `on` 必须报错, `auto` 必须回落并说出原因。
const NATIVE_REJECTED: &str = r#"
int main() { auto f = [](int x) { return x; }; return f(1); }
"#;

#[test]
fn help_documents_the_cxx_native_switch() {
    let out = dcc().arg("--help").output().expect("运行 dcc --help");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("--cxx-native"),
        "--help 没有列出 --cxx-native:\n{text}"
    );
}

#[test]
fn invalid_mode_is_a_hard_error() {
    let dir = temp_dir("invalid");
    let src = write_src(&dir, "plain.cpp", PLAIN_CLASS);
    let obj = dir.join("plain.o");
    for arg in ["--cxx-native=bogus", "--cxx-native"] {
        let out = dcc()
            .arg(arg)
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("运行 dcc");
        assert!(!out.status.success(), "{arg} 被接受了 (应当报错退出)");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("unknown C++ native mode") || err.contains("missing value after --cxx-native"),
            "{arg} 的错误信息不对: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn off_forces_legacy_with_visible_decision_and_summary() {
    let dir = temp_dir("off");
    let src = write_src(&dir, "plain.cpp", PLAIN_CLASS);
    let obj = dir.join("plain.o");
    let out = dcc()
        .arg("-v")
        .arg("--cxx-native=off")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("运行 dcc");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "off 模式编译失败:\n{err}");
    assert!(
        err.contains("c++ backend decision: path=legacy reason=--cxx-native=off forced legacy backend"),
        "verbose 没有暴露 off 的决策与原因:\n{err}"
    );
    assert!(
        err.contains("c++ path summary: native=0 legacy=1"),
        "路径统计没有计入 legacy:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_and_on_use_the_native_path() {
    for (tag, mode) in [("default", None), ("on", Some("--cxx-native=on"))] {
        let dir = temp_dir(&format!("native-{tag}"));
        let src = write_src(&dir, "plain.cpp", PLAIN_CLASS);
        let obj = dir.join("plain.o");
        let mut cmd = dcc();
        cmd.arg("-v");
        if let Some(m) = mode {
            cmd.arg(m);
        }
        let out = cmd
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("运行 dcc");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "[{tag}] 编译失败:\n{err}");
        assert!(
            err.contains("c++ backend decision: path=native"),
            "[{tag}] 没有走 native:\n{err}"
        );
        assert!(
            err.contains("c++ path summary: native=1 legacy=0"),
            "[{tag}] 路径统计没有计入 native:\n{err}"
        );
        assert!(obj.exists(), "[{tag}] 没有产出目标文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `on` 不得静默 fallback: native 接不住就是硬错误, 且绝不进 legacy。
#[test]
fn on_never_falls_back_silently() {
    let dir = temp_dir("on-nofallback");
    let src = write_src(&dir, "rej.cpp", NATIVE_REJECTED);
    let obj = dir.join("rej.o");
    let out = dcc()
        .arg("--cxx-native=on")
        .arg("-v")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("运行 dcc");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "on 模式静默接住了它不该支持的 TU:\n{err}");
    assert!(err.contains("E0500"), "on 模式必须报 E0500:\n{err}");
    assert!(
        !err.contains("using C++ legacy backend path"),
        "on 模式偷偷走了 legacy:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `auto` 的 fallback 必须在 verbose 里可见 (原因 + 实际走的路径)。
#[test]
fn auto_fallback_is_visible_in_verbose() {
    let dir = temp_dir("auto-fallback");
    let src = write_src(&dir, "rej.cpp", NATIVE_REJECTED);
    let obj = dir.join("rej.o");
    let out = dcc()
        .arg("--cxx-native=auto")
        .arg("-v")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("运行 dcc");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("c++ auto fallback: native lowering rejected TU:"),
        "auto 回落没有说出原因:\n{err}"
    );
    assert!(
        err.contains("using C++ legacy backend path after native rejection"),
        "auto 回落没有暴露实际路径:\n{err}"
    );
    assert!(
        err.contains("c++ path summary: native=0 legacy=1"),
        "auto 回落没有计入 fallback 统计:\n{err}"
    );
    assert!(
        err.contains("fallback[native lowering rejected TU:"),
        "fallback 原因没有进入统计摘要:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
