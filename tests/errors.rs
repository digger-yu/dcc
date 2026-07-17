// tests/errors.rs - 错误诊断 snapshot 回归（E0001–E1008）
//
// 每个用例：tests/fixtures/errors/<name>.c + <name>.stderr.snap
// 更新 snapshot：DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 编译模式
enum Mode {
    /// -fsyntax-only
    SyntaxOnly,
    /// -c -o <tmp.o>
    CompileObject,
}

/// 单个错误诊断用例
struct ErrorCase {
    name: &'static str,
    code: &'static str,
    mode: Mode,
}

const CASES: &[ErrorCase] = &[
    ErrorCase { name: "e0001_alignof", code: "E0001", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0002", code: "E0002", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0003", code: "E0003", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0004", code: "E0004", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0006", code: "E0006", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0009", code: "E0009", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0010", code: "E0010", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0011", code: "E0011", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0012", code: "E0012", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0013", code: "E0013", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0200", code: "E0200", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0201", code: "E0201", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0202", code: "E0202", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0203", code: "E0203", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0204", code: "E0204", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0206", code: "E0206", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0207", code: "E0207", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e1003", code: "E1003", mode: Mode::CompileObject },
    ErrorCase { name: "e1004", code: "E1004", mode: Mode::CompileObject },
    ErrorCase { name: "e1005", code: "E1005", mode: Mode::CompileObject },
    ErrorCase { name: "e1006", code: "E1006", mode: Mode::CompileObject },
    ErrorCase { name: "e1007", code: "E1007", mode: Mode::CompileObject },
    ErrorCase { name: "e1008", code: "E1008", mode: Mode::CompileObject },
    ErrorCase { name: "bad", code: "E0006", mode: Mode::SyntaxOnly },
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/errors")
}

/// 剥离终端 ANSI 转义序列
fn strip_ansi(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// 将 stderr 中的绝对路径替换为稳定占位符 `<file>`
fn normalize_stderr(raw: &str, file_path: &Path) -> String {
    let stripped = strip_ansi(raw);
    let canonical = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().into_owned();
    let mut normalized = stripped.replace(&canonical_str, "<file>");
    if cfg!(windows) {
        normalized = normalized.replace(&canonical_str.replace('\\', "/"), "<file>");
    }
    // 兜底：按文件名替换 ` --> …/name.c:line:col`
    if !normalized.contains("<file>") {
        if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
            for line in stripped.lines() {
                if let Some(rest) = line.strip_prefix("  --> ") {
                    if rest.contains(name) {
                        let mut parts: Vec<&str> = rest.rsplitn(3, ':').collect();
                        parts.reverse();
                        if parts.len() == 3 {
                            let loc = format!("{}:{}", parts[1], parts[2]);
                            normalized = normalized.replace(
                                &format!("  --> {rest}"),
                                &format!("  --> <file>:{loc}"),
                            );
                        }
                    }
                }
            }
        }
    }
    format!("{}\n", normalized.trim_end())
}

fn run_dcc(args: &[&str], input: &Path) -> (String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(args)
        .arg(input)
        .output()
        .expect("运行 dcc");
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (stderr, code)
}

fn snap_path(name: &str) -> PathBuf {
    fixtures_dir().join(format!("{name}.stderr.snap"))
}

fn assert_snapshot(name: &str, actual: &str) {
    let path = snap_path(name);
    if std::env::var("DCC_UPDATE_SNAPSHOTS").is_ok() {
        fs::write(&path, actual).expect("写入 snapshot");
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "缺少 snapshot {}: {e}\n运行 DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors 生成",
            path.display()
        );
    });
    assert_eq!(
        expected,
        actual,
        "snapshot 不匹配: {}\n运行 DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors 更新",
        name
    );
}

fn dcc_args_for(case: &ErrorCase) -> Vec<String> {
    let mut args = vec!["-std=gnu23".to_string()];
    match case.mode {
        Mode::SyntaxOnly => args.push("-fsyntax-only".to_string()),
        Mode::CompileObject => {
            args.push("-c".to_string());
            args.push("-o".to_string());
            args.push(
                std::env::temp_dir()
                    .join(format!("dcc-err-{}-{}.o", case.name, std::process::id()))
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    args
}

#[test]
fn diagnostic_error_snapshots() {
    let dir = fixtures_dir();
    for case in CASES {
        let source = dir.join(format!("{}.c", case.name));
        assert!(source.exists(), "缺少 fixture {}", source.display());

        let arg_strings = dcc_args_for(case);
        let args: Vec<&str> = arg_strings.iter().map(String::as_str).collect();
        let (stderr, status) = run_dcc(&args, &source);
        assert!(
            status != 0,
            "用例 {} 应编译失败 (exit={status})",
            case.name
        );
        assert!(
            stderr.contains(&format!("error[{}]", case.code)),
            "用例 {} 应包含 error[{}]，stderr:\n{stderr}",
            case.name,
            case.code
        );

        let normalized = normalize_stderr(&stderr, &source);
        assert_snapshot(case.name, &normalized);
    }
}

#[test]
fn e0100_no_input_file_cli() {
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .output()
        .expect("运行 dcc");
    assert!(!output.status.success());
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(
        stderr.contains("dcc: error: no input file"),
        "CLI 无输入文件应输出 dcc: error: …\n{stderr}"
    );
}

#[test]
fn e0100_cannot_read_file() {
    let missing = fixtures_dir().join("does_not_exist.c");
    let (stderr, status) = run_dcc(&["-std=gnu23", "-fsyntax-only"], &missing);
    assert!(status != 0);
    let normalized = normalize_stderr(&stderr, &missing);
    assert!(normalized.contains("error[E0100]:"));
    assert!(normalized.contains("cannot read"));
}

#[test]
fn diagnostic_format_has_location_and_caret() {
    let source = fixtures_dir().join("e0004.c");
    let (stderr, _) = run_dcc(&["-std=gnu23", "-fsyntax-only"], &source);
    let normalized = normalize_stderr(&stderr, &source);
    assert!(normalized.contains("error[E0004]:"));
    assert!(normalized.contains(" --> <file>:"));
    assert!(normalized.contains(" | "));
    assert!(normalized.contains('^'));
    assert!(normalized.contains("hint:"));
}
