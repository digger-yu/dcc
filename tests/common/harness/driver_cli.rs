//! 驱动 CLI 面助手：待测 `dcc` 可执行入口、临时源文件/目录、夹具根目录，
//! 以及 C++ 后端**路径选择**（`--cxx-native=auto|off|on`）契约用例需要的两类源文件。
//!
//! 由 `tests/common/harness.rs` 挂载，经 `pub(crate) use` 注入 integration crate root，
//! 供 `tests/integration/cpp_runtime.rs` 的路径契约测试与 `tests/profile.rs` 复用
//! （profile 不再自带一份 `dcc()` / 夹具路径逻辑）。这里只放“驱动 / 命令行”侧的
//! 助手；AST/HIR 侧的对照助手在 `cpp_frontend.rs`。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 待测 dcc 可执行文件（`CARGO_BIN_EXE_dcc` 由 cargo 注入，指向本次构建产物）。
///
/// 名字不叫 `dcc()`：并入 integration crate root 后，`dcc` 同时是 lib crate 名，
/// 会与它撞名（`dcc::cpp::…` 路径解析优先于 glob 导入的同名函数）。
pub(crate) fn dcc_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dcc"))
}

/// 夹具根目录（`tests/fixtures`）。运行时路径一律基于 `CARGO_MANIFEST_DIR`，
/// 与源文件位置无关（§5.2 规则 4）。
pub(crate) fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// 写一个临时源文件, 返回其路径。
pub(crate) fn write_src(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).expect("写入临时源文件");
    p
}

/// 每个用例一个独立临时目录（带 pid 后缀，避免并行用例互踩）。
pub(crate) fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dcc-cxx-native-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

/// native 能力矩阵认可的普通类 TU：`off` 必须走 legacy，`on`/默认必须走 native。
pub(crate) const PLAIN_CLASS: &str = r#"
struct P { int x; int get() const { return x; } };
int main() { P p; p.x = 3; return p.get(); }
"#;

/// native 的 lowering 会拒绝、而能力矩阵不会预先拦下的写法 (闭包)。
/// `on` 必须报错, `auto` 必须回落并说出原因。
pub(crate) const NATIVE_REJECTED: &str = r#"
int main() { auto f = [](int x) { return x; }; return f(1); }
"#;
