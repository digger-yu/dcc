// tests/errors.rs - 错误诊断 snapshot 回归
//
// 错误码详细说明: docs/DIAGNOSTICS.md
// 每个用例：tests/fixtures/errors/<name>.c + <name>.stderr.snap
// 更新 snapshot：DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors
//
// 诊断格式版本：变更 GitHub 风格四段式布局时递增并全量刷新 snapshot
const DIAGNOSTIC_FORMAT_VERSION: u32 = 1;

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
    ErrorCase { name: "e0005", code: "E0005", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0006", code: "E0006", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0007", code: "E0007", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e0008", code: "E0008", mode: Mode::SyntaxOnly },
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
    ErrorCase { name: "e0208_embed", code: "E0208", mode: Mode::SyntaxOnly },
    ErrorCase { name: "e1001", code: "E1001", mode: Mode::CompileObject },
    ErrorCase { name: "e1002", code: "E1002", mode: Mode::CompileObject },
    ErrorCase { name: "e1003", code: "E1003", mode: Mode::CompileObject },
    ErrorCase { name: "e1004", code: "E1004", mode: Mode::CompileObject },
    ErrorCase { name: "e1005", code: "E1005", mode: Mode::CompileObject },
    ErrorCase { name: "e1006", code: "E1006", mode: Mode::CompileObject },
    ErrorCase { name: "e1007", code: "E1007", mode: Mode::CompileObject },
    ErrorCase { name: "e1008", code: "E1008", mode: Mode::CompileObject },
    ErrorCase { name: "e1009", code: "E1009", mode: Mode::CompileObject },
    ErrorCase { name: "e1010", code: "E1010", mode: Mode::CompileObject },
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
    let stripped = strip_ansi(raw).replace("\r\n", "\n").replace('\r', "\n");
    let canonical = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().into_owned();
    let mut normalized = stripped.replace(&canonical_str, "<file>");
    if cfg!(windows) {
        normalized = normalized.replace(&canonical_str.replace('\\', "/"), "<file>");
    }
    normalized = normalized.replace("secret.h", "<secret.h>");
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
    let normalized: String = normalized
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .collect::<Vec<_>>()
        .join("\n");
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

/// 校验 snapshot 是否符合锁定的 GitHub 风格诊断格式
fn validate_diagnostic_snapshot(text: &str, name: &str) -> Result<(), String> {
    let body = text.trim_end();
    if body.is_empty() {
        return Err(format!("{name}: snapshot 为空"));
    }
    let lines: Vec<&str> = body.lines().collect();
    let head = lines[0];
    let head_ok = head.starts_with("error[")
        || head.starts_with("warning[")
        || head.starts_with("note[")
        || head.starts_with("dcc: error:");
    if !head_ok {
        return Err(format!("{name}: 首行应为 error[…]/warning[…]/dcc: error:"));
    }
    if head.starts_with("error[") || head.starts_with("warning[") || head.starts_with("note[") {
        if !lines.iter().any(|l| l.contains(" --> ") && l.matches(':').count() >= 2) {
            return Err(format!("{name}: 缺少位置行 ` --> file:line:col`"));
        }
    }
    Ok(())
}

fn assert_snapshot(name: &str, actual: &str) {
    if std::env::var("DCC_UPDATE_SNAPSHOTS").is_ok() {
        fs::write(snap_path(name), actual).expect("写入 snapshot");
        return;
    }
    validate_diagnostic_snapshot(actual, name)
        .unwrap_or_else(|e| panic!("snapshot 格式校验失败: {e}"));
    let path = snap_path(name);
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "缺少 snapshot {}: {e}\n运行 DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors 生成",
            path.display()
        );
    });
    validate_diagnostic_snapshot(&expected, name)
        .unwrap_or_else(|e| panic!("已存 snapshot 格式损坏 ({}): {e}", path.display()));
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

fn stderr_text(output: &str) -> String {
    strip_ansi(output)
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
        let plain = stderr_text(&stderr);
        assert!(
            status != 0,
            "用例 {} 应编译失败 (exit={status})",
            case.name
        );
        assert!(
            plain.contains(&format!("error[{}]", case.code)),
            "用例 {} 应包含 error[{}]，stderr:\n{plain}",
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
fn w0001_implicit_int_warning_snapshot() {
    let source = fixtures_dir().join("w0001.c");
    let (stderr, status) = run_dcc(&["-std=gnu23", "-fsyntax-only"], &source);
    let plain = stderr_text(&stderr);
    assert!(
        status == 0,
        "W0001 隐式 int 仅警告，应编译成功，stderr:\n{plain}"
    );
    assert!(plain.contains("warning[W0001]:"), "stderr:\n{plain}");
    let normalized = normalize_stderr(&stderr, &source);
    assert_snapshot("w0001", &normalized);
}

#[cfg(unix)]
#[test]
fn e0205_unreadable_include_snapshot() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixtures_dir();
    let secret = dir.join("secret.h");
    fs::write(&secret, "int secret;\n").expect("write secret.h");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");

    let source = dir.join("e0205.c");
    let (stderr, status) = run_dcc(
        &["-std=gnu23", "-fsyntax-only", "-I", dir.to_str().unwrap()],
        &source,
    );

    let _ = fs::remove_file(&secret);
    assert!(status != 0);
    assert!(stderr_text(&stderr).contains("error[E0205]:"), "stderr:\n{stderr}");
    let normalized = normalize_stderr(&stderr, &source);
    assert_snapshot("e0205", &normalized);
}

#[cfg(unix)]
#[test]
fn e0209_force_include_unreadable_snapshot() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixtures_dir();
    let secret = dir.join("secret.h");
    fs::write(&secret, "int secret;\n").expect("write secret.h");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");

    let source = dir.join("e0209.c");
    let (stderr, status) = run_dcc(
        &[
            "-std=gnu23",
            "-fsyntax-only",
            "-include",
            secret.to_str().unwrap(),
        ],
        &source,
    );

    let _ = fs::remove_file(&secret);
    assert!(status != 0);
    assert!(stderr_text(&stderr).contains("error[E0209]:"), "stderr:\n{stderr}");
    let normalized = normalize_stderr(&stderr, &source);
    assert_snapshot("e0209", &normalized);
}

#[test]
fn e0208_invalid_utf8_snapshot() {
    let dir = fixtures_dir();
    let invalid = dir.join(format!("e0208_utf8_{}.c", std::process::id()));
    fs::write(&invalid, b"int x;\n\xFF\n").expect("write invalid utf8 source");

    let (stderr, status) = run_dcc(&["-std=gnu23", "-fsyntax-only"], &invalid);
    let plain = stderr_text(&stderr);
    let normalized = normalize_stderr(&stderr, &invalid);
    let _ = fs::remove_file(&invalid);

    assert!(status != 0);
    assert!(plain.contains("error[E0208]:"), "stderr:\n{plain}");
    assert!(plain.contains("UTF-8"), "stderr:\n{plain}");
    assert_snapshot("e0208_utf8", &normalized);
}

#[test]
fn diagnostic_format_locked_snapshot() {
    let source = fixtures_dir().join("e0004.c");
    let (stderr, status) = run_dcc(&["-std=gnu23", "-fsyntax-only"], &source);
    assert!(status != 0);
    let normalized = normalize_stderr(&stderr, &source);
    assert_snapshot("diagnostic_format", &normalized);
}

#[test]
fn diagnostic_format_version_is_current() {
    // 变更 diagnostics.rs 输出布局时：递增 DIAGNOSTIC_FORMAT_VERSION 并刷新全部 snapshot
    assert_eq!(DIAGNOSTIC_FORMAT_VERSION, 1);
}

#[test]
fn all_snapshots_match_locked_schema() {
    if std::env::var("DCC_UPDATE_SNAPSHOTS").is_ok() {
        return;
    }
    let dir = fixtures_dir();
    for entry in fs::read_dir(&dir).expect("read fixtures/errors") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("snap") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read snap");
        let name = path.file_stem().expect("stem").to_string_lossy();
        validate_diagnostic_snapshot(&text, &name)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

#[test]
fn cases_have_matching_fixtures_and_snapshots() {
    let dir = fixtures_dir();
    for case in CASES {
        let source = dir.join(format!("{}.c", case.name));
        assert!(source.is_file(), "CASES 缺少 fixture {}", source.display());
        let snap = snap_path(case.name);
        if std::env::var("DCC_UPDATE_SNAPSHOTS").is_err() {
            assert!(
                snap.is_file(),
                "CASES 缺少 snapshot {}（运行 DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors 生成）",
                snap.display()
            );
        }
    }
}

#[test]
fn cli_multiple_sources_error() {
    let dir = fixtures_dir();
    let a = dir.join("e0004.c");
    let b = dir.join("e0006.c");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-o", "a.out", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("运行 dcc");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(
        stderr.contains("dcc: error: multiple source files cannot produce a single executable"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn cli_relocatable_requires_object_inputs() {
    let source = fixtures_dir().join("e0004.c");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-std=gnu23",
            "-r",
            "-o",
            "merged.o",
            source.to_str().unwrap(),
        ])
        .output()
        .expect("运行 dcc");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    assert!(
        stderr.contains("dcc: error: relocatable link (-r) requires object (.o) inputs"),
        "stderr:\n{stderr}"
    );
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
