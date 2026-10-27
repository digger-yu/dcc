// included from tests/integration.rs — not a standalone cargo target

/// build_static.sh script should exist and be executable
#[test]
#[cfg(unix)]
fn static_build_script_exists() {
    let script_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build_static.sh");
    assert!(script_path.exists(), "scripts/build_static.sh should exist");
    let metadata = fs::metadata(&script_path).expect("read script metadata");
    let mode = metadata.permissions().mode();
    assert!(mode & 0o111 != 0, "build_static.sh should be executable");
}

#[test]
#[cfg(unix)]
fn host_wrapper_rejects_unknown_wp_option() {
    let script_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/dcc-host-wrapper.sh");
    let output = Command::new("bash")
        .arg(script_path)
        .args(["-c", "-Wp,--silently-lost", "input.c"])
        .output()
        .expect("run dcc-host-wrapper.sh");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("unsupported preprocessor option in -Wp"));
}

#[test]
#[cfg(unix)]
fn host_wrapper_accepts_whitelisted_wp_options() {
    let script_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/dcc-host-wrapper.sh");
    let output = Command::new("bash")
        .arg(script_path)
        .args(["-c", "-Wp,-MMD,/tmp/main.d,-MT,main.o,-MP", "input.c"])
        .env("DCC_BIN", "echo")
        .output()
        .expect("run dcc-host-wrapper.sh");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("-MD"), "stdout: {stdout}");
    assert!(stdout.contains("-MF"), "stdout: {stdout}");
    assert!(stdout.contains("/tmp/main.d"), "stdout: {stdout}");
    assert!(stdout.contains("-MT"), "stdout: {stdout}");
    assert!(stdout.contains("main.o"), "stdout: {stdout}");
    assert!(stdout.contains("-MP"), "stdout: {stdout}");
}

/// 严格类型检查: 隐式指针/整数转换 warning (不阻塞)
#[test]
fn m15_strict_warning_compile() {
    let src = include_str!("../fixtures/c/strict_warning.c");
    let path = std::env::temp_dir().join(format!("dcc-m15-strict-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-Wint-conversion", "-Wint-truncation", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&path);
    assert!(
        output.status.success(),
        "strict-warn should not error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// M2.6: batch compilation test - compile a representative set
/// of existing C fixtures to .o files, verifying the compiler
/// handles diverse C programs (asm, attributes, builtins, etc.).
#[test]
#[cfg(unix)]
fn m2_6_batch_compile() {
    let fixtures = [
        "tests/fixtures/smoke/hello.c",
        "tests/fixtures/smoke/arith.c",
        "tests/fixtures/smoke/control_flow.c",
        "tests/fixtures/smoke/fib.c",
        "tests/fixtures/smoke/basic_struct.c",
        "tests/fixtures/smoke/switch.c",
        "tests/fixtures/smoke/function_call.c",
        "tests/fixtures/smoke/bitfield.c",
        "tests/fixtures/smoke/global_array.c",
        "tests/fixtures/smoke/logic.c",
        "tests/fixtures/smoke/rel_ops.c",
        "tests/fixtures/smoke/pointer_arith.c",
    ];
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let mut pass = 0u32;
    let mut fail = 0u32;
    let dir = std::env::temp_dir();
    for src in &fixtures {
        // smoke 夹具为必备文件, 缺失即硬失败 (不再静默 continue 跳过)
        assert!(std::path::Path::new(src).exists(), "smoke fixture missing: {src}");
        // 路径含子目录 (tests/fixtures/smoke/...), 拍平避免父目录不存在
        let flat = src.replace(['/', '\\'], "-");
        let obj = dir.join(format!("dcc-m2-6-{flat}-{}.o", std::process::id()));
        let out = Command::new(dcc)
            .args(["-c", "-o"])
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("dcc");
        let _ = fs::remove_file(&obj);
        if out.status.success() {
            pass += 1;
        } else {
            fail += 1;
            eprintln!("FAIL {src}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    assert!(fail == 0, "batch compile: {pass} passed, {fail} failed");
}

// ============================================================================
// #error/#warning multi-format diagnostic snapshot tests
// ============================================================================

/// #error 诊断: GitHub 默认格式应包含 caret 和 source line
#[test]
fn error_diag_github_format_caret_and_source() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-err-github-{}.c", std::process::id()));
    fs::write(&src, "#error this is a test error\nint x = 1;\n").expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg(&src)
        .output()
        .expect("run dcc");

    let _ = fs::remove_file(&src);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "#error must fail");
    // GitHub 默认格式: 应有 --> 指向源码行
    assert!(stderr.contains("-->"), "github format should have caret: {stderr}");
    assert!(stderr.contains("E0203"), "should have E0203 code: {stderr}");
    assert!(stderr.contains("this is a test error"), "msg should be included: {stderr}");
}

/// #error 诊断: --diagnostics-format=msvc 应输出 file(line,col): error 格式
#[test]
fn error_diag_msvc_format() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-err-msvc-{}.c", std::process::id()));
    fs::write(&src, "#error msvc format test\n").expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg("-fdiagnostics-format=msvc")
        .arg(&src)
        .output()
        .expect("run dcc");

    let _ = fs::remove_file(&src);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "#error must fail");
    // MSVC 格式: file(line,col): error CODE: msg
    assert!(
        stderr.contains("error[E0203]") || stderr.contains(": error E0203:"),
        "msvc format should have error code: {stderr}"
    );
}

/// #error 诊断: --diagnostics-format=clang 应输出 caret 诊断
#[test]
fn error_diag_clang_format() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-err-clang-{}.c", std::process::id()));
    fs::write(&src, "#error clang format test\n").expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg("-fdiagnostics-format=clang")
        .arg(&src)
        .output()
        .expect("run dcc");

    let _ = fs::remove_file(&src);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "#error must fail");
    // clang 格式应有 caret 行
    assert!(stderr.contains('^'), "clang format should have caret: {stderr}");
}

/// #warning 诊断: 多格式下均为 warning 不阻断编译
#[test]
fn warning_does_not_block_compilation() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-warn-{}.c", std::process::id()));
    fs::write(&src, "#warning this is a warning\nint x = 1;\n").expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg(&src)
        .output()
        .expect("run dcc");

    let _ = fs::remove_file(&src);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "#warning must not fail");
    assert!(stderr.contains("W0200"), "should have W0200 code: {stderr}");
}

#[test]
fn unsupported_z_options_are_errors() {
    for z in ["origin", "unknown-dcc-option"] {
        let (output, exe) = run_compile_and_run_flags(
            &format!("bad-z-{z}"),
            "int main(void) { return 0; }",
            &["-fbuiltin-linker", "-z", z],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_file(&exe);
        assert!(!output.status.success(), "-z {z} should fail");
        assert!(
            stderr.contains("unsupported -z option"),
            "missing unsupported -z diagnostic for {z}: {stderr}"
        );
    }
}

/// Feature 15: -fno-common (tentative definitions)
#[test]
#[cfg(unix)]
fn no_common_mode() {
    let src = r#"
int global_var;
int get_global(void) { return global_var; }
"#;
    let tmp = std::env::temp_dir();
    let src_path = tmp.join("no_common.c");
    let obj_path = tmp.join("no_common.o");
    fs::write(&src_path, src).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fno-common", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -fno-common");
    assert!(
        output.status.success(),
        "no-common compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

/// Unknown -m*/-f* flags should be silently accepted (GCC 兼容)
/// Linux 内核 Makefile 使用大量 GCC 特有选项, dcc 安全忽略未知选项以避免噪音.
#[test]
#[cfg(unix)]
fn unknown_flags_emit_warnings() {
    let src = "int main(void) { return 0; }\n";
    let tmp = std::env::temp_dir();
    let src_path = tmp.join("unknown_flags.c");
    let obj_path = tmp.join("unknown_flags.o");
    fs::write(&src_path, src).unwrap();

    // Unknown -m* flag should be silently accepted (GCC 兼容)
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-munknown-future-flag", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc with unknown -m flag");
    assert!(output.status.success(), "compile should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ignoring unknown option"),
        "unknown -m flag should be silently accepted, stderr: {}",
        stderr
    );
    let _ = fs::remove_file(&obj_path);

    // Unknown -f* flag should be silently accepted (GCC 兼容)
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-funknown-future-flag", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc with unknown -f flag");
    assert!(output.status.success(), "compile should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ignoring unknown option"),
        "unknown -f flag should be silently accepted, stderr: {}",
        stderr
    );
    let _ = fs::remove_file(&obj_path);

    // Known no-op flags should NOT emit warnings
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-fno-plt",
            "-falign-jumps=1",
            "-falign-loops=1",
            "-mpreferred-stack-boundary=2",
            "-o",
        ])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc with known no-op flags");
    assert!(output.status.success(), "compile should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ignoring unknown option"),
        "known no-op flags should not warn, stderr: {}",
        stderr
    );
    let _ = fs::remove_file(&obj_path);

    // Non-default stack boundary still warns (not implemented).
    // -falign-jumps/-falign-loops 现已实现 (NOP 填充, 见 emit_align_padding),
    // 非 1 值不再发警告.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-mpreferred-stack-boundary=4",
            "-falign-jumps=16",
            "-falign-loops=8",
            "-o",
        ])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc with non-default alignment values");
    assert!(output.status.success(), "compile should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DCC does not adjust stack alignment"),
        "should warn about non-default stack boundary, stderr: {}",
        stderr
    );
    assert!(
        !stderr.contains("-falign-jumps=16"),
        "jump alignment is implemented, should not warn, stderr: {}",
        stderr
    );
    assert!(
        !stderr.contains("-falign-loops=8"),
        "loop alignment is implemented, should not warn, stderr: {}",
        stderr
    );
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

/// -j flag parsing: -j0 is rejected, -j1/-j4/--jobs=2 are accepted.
#[test]
fn jobs_flag_parsing() {
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-jobs-{pid}.c"));
    fs::write(&src, "int f(void) { return 0; }\n").expect("write");

    // -j0 must be rejected
    let out = Command::new(dcc)
        .args(["-j0", "-c", src.to_str().unwrap()])
        .output()
        .expect("run dcc");
    assert!(!out.status.success(), "-j0 should be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-j must be >= 1"), "expected -j error, got: {stderr}");

    // -j1 must succeed (sequential)
    // 输出路径用临时文件而不是 /dev/null（原因见 harness::discard_out），用完删掉。
    let out1 = discard_out("jobs-j1");
    let out = Command::new(dcc)
        .args(["-j1", "-c", "-o"])
        .arg(&out1)
        .arg(src.to_str().unwrap())
        .output()
        .expect("run dcc");
    assert!(
        out.status.success(),
        "-j1 should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out1.exists(), "-j1 必须真的产出对象文件");
    let _ = fs::remove_file(&out1);

    // -j4 must succeed
    let out4 = discard_out("jobs-j4");
    let out = Command::new(dcc)
        .args(["-j4", "-c", "-o"])
        .arg(&out4)
        .arg(src.to_str().unwrap())
        .output()
        .expect("run dcc");
    assert!(
        out.status.success(),
        "-j4 should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out4.exists(), "-j4 必须真的产出对象文件");
    let _ = fs::remove_file(&out4);

    // --jobs=2 must succeed
    let outj = discard_out("jobs-eq2");
    let out = Command::new(dcc)
        .args(["--jobs=2", "-c", "-o"])
        .arg(&outj)
        .arg(src.to_str().unwrap())
        .output()
        .expect("run dcc");
    assert!(
        out.status.success(),
        "--jobs=2 should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(outj.exists(), "--jobs=2 必须真的产出对象文件");
    let _ = fs::remove_file(&outj);

    let _ = fs::remove_file(&src);
}

#[test]
fn response_file_with_comments_and_quotes() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-rsp-q-{}.c", std::process::id()));
    let rsp = dir.join(format!("dcc-rsp-q-{}.rsp", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    // Response file with comments and quoted args
    fs::write(&rsp, format!("# This is a comment\n-fsyntax-only\n\"{}\"\n", src.display()))
        .expect("write rsp");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("@{}", rsp.display()))
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&rsp);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// M5: Bootstrap selftest — compile and run the bootstrap verification
/// program that exercises 14 test groups (structs, function pointers,
/// recursion, linked lists, bit ops, enums/unions, strings, long long,
/// inline asm, variadic macros, _Static_assert, statement expressions,
/// goto, switch range, static arrays).
#[test]
fn m5_bootstrap_selftest() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c/bootstrap_selftest.c");
    if !fixture.exists() {
        return; // fixture not present, skip
    }
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-bootstrap-selftest-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O0", "-std=gnu23", "-o"])
        .arg(&exe)
        .arg(&fixture)
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "bootstrap selftest compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
    }
    let run = Command::new(&exe).output().expect("run bootstrap selftest");
    let _ = fs::remove_file(&exe);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "bootstrap selftest failed: {stdout}");
    assert!(stdout.contains("ALL 14 TEST GROUPS PASSED"), "unexpected output: {stdout}");
}

/// M5: Bootstrap determinism — compile the same source twice to .o and
/// verify byte-level identical output (deterministic codegen).
#[test]
fn m5_bootstrap_determinism() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c/bootstrap_selftest.c");
    if !fixture.exists() {
        return;
    }
    let dir = std::env::temp_dir();
    let o1 = dir.join(format!("dcc-bootstrap-det1-{}.o", std::process::id()));
    let o2 = dir.join(format!("dcc-bootstrap-det2-{}.o", std::process::id()));
    for (src, out) in [(&fixture, &o1), (&fixture, &o2)] {
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-O0", "-std=gnu23", "-o"])
            .arg(out)
            .arg(src)
            .output()
            .expect("run dcc");
        assert!(
            output.status.success(),
            "determinism compile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let b1 = fs::read(&o1).expect("read o1");
    let b2 = fs::read(&o2).expect("read o2");
    let _ = fs::remove_file(&o1);
    let _ = fs::remove_file(&o2);
    assert_eq!(
        b1,
        b2,
        "bootstrap determinism: .o files differ ({}B vs {}B)",
        b1.len(),
        b2.len()
    );
}

#[test]
fn response_file_at_file_expands_arguments() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-rsp-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-rsp-{}.o", std::process::id()));
    let rsp = dir.join(format!("dcc-rsp-{}.rsp", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    // Response file contains: -c -o <obj> <src>
    fs::write(&rsp, format!("-c\n-o\n{}\n{}\n", obj.display(), src.display())).expect("write rsp");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("@{}", rsp.display()))
        .output()
        .expect("run dcc");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(obj.exists(), "response file should produce .o output");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&rsp);
}

#[test]
fn pipe_input_c_mode_stdin_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"int main(void) { return 0; }\n")
                .ok();
            child.wait_with_output()
        })
        .expect("run dcc -c -o - -");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // Verify output is ELF (starts with 0x7f 'E' 'L' 'F')
    let out = &output.stdout;
    assert!(
        out.len() > 4 && out[0] == 0x7f && out[1] == b'E' && out[2] == b'L' && out[3] == b'F',
        "output should be ELF binary, got {} bytes",
        out.len()
    );
}

#[test]
fn pipe_input_e_mode_stdin_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"int main(void) { return 0; }\n")
                .ok();
            child.wait_with_output()
        })
        .expect("run dcc -E -");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("int main"), "preprocessed output should contain source");
}

#[test]
fn pipe_input_s_mode_stdin_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"int main(void) { return 0; }\n")
                .ok();
            child.wait_with_output()
        })
        .expect("run dcc -S -o - -");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(".text") && stdout.contains("main"),
        "output should be assembly with .text and main"
    );
}

#[test]
fn pipe_input_default_output_name_is_a_o() {
    let dir = std::env::temp_dir();
    let work = dir.join(format!("dcc-pipe-default-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-"])
        .current_dir(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"int main(void) { return 0; }\n")
                .ok();
            child.wait_with_output()
        })
        .expect("run dcc -c -");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // GCC: `dcc -c -` produces `a.o` in the current directory
    assert!(work.join("a.o").exists(), "default output should be a.o");
    let _ = fs::remove_dir_all(&work);
}

/// Multi-file -c with -j4: compile 4 source files in parallel,
/// verify all .o files are produced and are valid ELF.
#[test]
fn multi_file_parallel_compile() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-mfpar-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir");

    // Create 4 source files
    for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
        let path = work.join(format!("{name}.c"));
        fs::write(&path, format!("int func_{i}(void) {{ return {i}; }}\n")).expect("write source");
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.arg("-c").arg("-j4").current_dir(&work);
    for name in &["a", "b", "c", "d"] {
        cmd.arg(format!("{name}.c"));
    }
    let out = cmd.output().expect("run dcc");
    assert!(
        out.status.success(),
        "multi-file parallel compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Verify all .o files exist and are valid ELF
    for name in &["a", "b", "c", "d"] {
        let obj = work.join(format!("{name}.o"));
        assert!(obj.exists(), "{name}.o not produced");
        let bytes = fs::read(&obj).expect("read .o");
        assert!(
            bytes.len() > 4 && bytes[0..4] == [0x7f, b'E', b'L', b'F'],
            "{name}.o is not valid ELF"
        );
    }

    let _ = fs::remove_dir_all(&work);
}

/// Parallel (-j4) vs sequential (-j1) produce byte-identical .o files.
#[test]
fn multi_file_parallel_byte_identical() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work_par = dir.join(format!("dcc-mfpar-par-{pid}"));
    let work_seq = dir.join(format!("dcc-mfpar-seq-{pid}"));
    let _ = fs::remove_dir_all(&work_par);
    let _ = fs::remove_dir_all(&work_seq);
    fs::create_dir_all(&work_par).expect("mkdir par");
    fs::create_dir_all(&work_seq).expect("mkdir seq");

    // Create identical source files in both dirs
    for work in &[&work_par, &work_seq] {
        for (i, name) in ["x", "y", "z"].iter().enumerate() {
            let path = work.join(format!("{name}.c"));
            fs::write(
                &path,
                format!(
                    "int helper_{i}(int a) {{ return a + {i}; }}\n\
                     int func_{i}(void) {{ return helper_{i}({i}); }}\n"
                ),
            )
            .expect("write source");
        }
    }

    let compile = |work: &std::path::Path, jobs: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
        cmd.arg("-c").arg(format!("-j{jobs}")).current_dir(work);
        for name in &["x", "y", "z"] {
            cmd.arg(format!("{name}.c"));
        }
        let out = cmd.output().expect("run dcc");
        assert!(
            out.status.success(),
            "compile -j{jobs} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    compile(&work_par, "4");
    compile(&work_seq, "1");

    // Compare byte-for-byte
    for name in &["x", "y", "z"] {
        let par = fs::read(work_par.join(format!("{name}.o"))).expect("read par");
        let seq = fs::read(work_seq.join(format!("{name}.o"))).expect("read seq");
        assert_eq!(par, seq, "{name}.o: parallel and sequential differ");
    }

    let _ = fs::remove_dir_all(&work_par);
    let _ = fs::remove_dir_all(&work_seq);
}

/// -o with multiple inputs is rejected (GCC behavior).
#[test]
fn multi_file_rejects_output_flag() {
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-mfout-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir");
    fs::write(work.join("a.c"), "int a(void){return 0;}\n").expect("write a");
    fs::write(work.join("b.c"), "int b(void){return 0;}\n").expect("write b");

    let out = Command::new(dcc)
        .args(["-c", "-o", "combined.o", "a.c", "b.c"])
        .current_dir(&work)
        .output()
        .expect("run dcc");
    assert!(!out.status.success(), "-o with multiple inputs should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot specify -o with multiple input files"),
        "expected -o error, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&work);
}

/// 重复 `-O` 必须**最后一个生效**（与 gcc 同口径），并且不同档的产物确实有区别。
///
/// 为什么必须有这条：Lua 的 `src/Makefile` 把优化号写在用户标志**之前**
/// （`CFLAGS= -O2 -Wall -Wextra $(SYSCFLAGS) $(MYCFLAGS)`），所以 `MYCFLAGS="-O0"`
/// 递到编译器手上是一条带两个 `-O` 的命令行（实测：`… -O2 -Wall -Wextra -std=gnu99 -O0 -c -o lapi.o lapi.c`）。
/// 只有「后出现者胜」成立，“四个级别都测过”才不是“其实全是 -O2”。同时必须验
/// 一档之间**字节不同**：否则若优化器完全忽略 `-O`，下面那组等式会空对空成立。
#[test]
#[cfg(unix)]
fn repeated_opt_flag_last_wins() {
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-optlast-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("mkdir");
    // 一个会被优化掉的东西：循环 + 乘法，-O0 与 -O2 的产物必然不同
    fs::write(
        work.join("l.c"),
        "long f(long n){ long s=0; for(long i=0;i<n;i++) s+=i*2; return s; }\n",
    )
    .expect("write l.c");

    let build = |name: &str, flags: &[&str]| -> Vec<u8> {
        let out = Command::new(dcc)
            .args(flags)
            .args(["-c", "-o", name, "l.c"])
            .current_dir(&work)
            .output()
            .unwrap_or_else(|_| panic!("run dcc {flags:?}"));
        assert!(
            out.status.success(),
            "dcc {flags:?} 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fs::read(work.join(name)).unwrap_or_else(|_| panic!("读 {name}"))
    };

    let o0 = build("o0.o", &["-O0"]);
    let o2 = build("o2.o", &["-O2"]);
    let o2o0 = build("o2o0.o", &["-O2", "-O0"]);
    let o0o2 = build("o0o2.o", &["-O0", "-O2"]);
    let o2o0o0 = build("o2o0o0.o", &["-O2", "-O0", "-O0"]);
    let _ = fs::remove_dir_all(&work);

    assert_ne!(
        o0, o2,
        "-O0 与 -O2 产物逐字节相同 ⇒ 优化级别根本没作用，下面那组等式不成立"
    );
    assert_eq!(o2o0, o0, "`-O2 -O0` 必须等于 `-O0`（后出现的 -O 胜）");
    assert_eq!(
        o0o2, o2,
        "`-O0 -O2` 必须等于 `-O2`（后出现的胜；既不是取首个也不是取最大）"
    );
    assert_eq!(o2o0o0, o0, "三个 -O 时仍以最后一个为准");
}
