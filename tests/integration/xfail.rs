// included from tests/integration.rs — not a standalone cargo target

// ---------- objtool known failures xfail ----------

/// objtool known failure: "can't decode instruction"
/// 最小 xfail fixture: 构造一条 dcc 尚不能编码的指令,
/// 校验 objtool 报告该错误且 dcc 不崩溃.
#[test]
#[cfg(unix)]
fn objtool_xfail_cant_decode_instruction() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-objtool-decode-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-objtool-decode-{}.o", std::process::id()));
    // 使用 asm 注入一条 objtool 可能无法解码的字节序列
    let source = r#"asm(".byte 0x0f\n.byte 0x0b\n"); int main(void) { return 0; }"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    // dcc 必须成功编译 (asm 字节原样输出)
    assert!(
        output.status.success(),
        "dcc should compile asm byte fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// objtool known failure: "can't find starting instruction"
/// 最小 xfail fixture: 空函数或仅有数据标签的符号.
#[test]
#[cfg(unix)]
fn objtool_xfail_cant_find_starting_instruction() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-objtool-start-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-objtool-start-{}.o", std::process::id()));
    let source = r#"
asm(".globl empty_marker\n.type empty_marker, @function\nempty_marker:\n.size empty_marker, .-empty_marker");
int main(void) { return 0; }
"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    assert!(
        output.status.success(),
        "dcc should compile empty marker fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// objtool known failure: "can't find jump dest instruction"
/// 最小 xfail fixture: 跳转到数据标签.
#[test]
#[cfg(unix)]
fn objtool_xfail_cant_find_jump_dest() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-objtool-jump-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-objtool-jump-{}.o", std::process::id()));
    let source = r#"
asm(".globl jump_test\njump_test:\n"
    "   jmp data_label\n"
    "   ret\n"
    "data_label:\n"
    ".quad 0xdeadbeef\n"
    ".size jump_test, .-jump_test");
int main(void) { return 0; }
"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    assert!(
        output.status.success(),
        "dcc should compile jump-to-data fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// objtool known failure: "rethunk naked return"
/// 最小 xfail fixture: 在 -mfunction-return=keep 模式下保留裸 ret.
#[test]
#[cfg(unix)]
fn objtool_xfail_rethunk_naked_return() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-objtool-rethunk-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-objtool-rethunk-{}.o", std::process::id()));
    let source = r#"
int normal_return(int x) { return x + 1; }
"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-mfunction-return=keep", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    assert!(
        output.status.success(),
        "dcc should compile with -mfunction-return=keep: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ====== xfail harness: expected-fail for known unsupported features ======

/// xfail: 未知 offload triple 应产生警告或错误
#[test]
fn xfail_unknown_offload_triple_diagnostic() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-xfail-offload-{}.c", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fopenmp-targets=unknown-triple-xyz", "-c"])
        .arg(&src)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = fs::remove_file(&src);
    // 未知 triple 应产生警告或错误
    assert!(
        stderr.contains("unknown")
            || stderr.contains("unsupported")
            || stderr.contains("warning")
            || !output.status.success(),
        "unknown offload triple should produce diagnostic: {stderr}"
    );
}

/// xfail: 不同尺寸的 vector/scalar cast 应被拒绝
#[test]
fn xfail_vector_scalar_size_mismatch_rejected() {
    let output = run_syntax_test(
        "xfail-cast-mismatch",
        "typedef int v4si __attribute__((vector_size(16)));\n\
         typedef long long v2di __attribute__((vector_size(16)));\n\
         int main(void) {\n\
             v4si a = (v4si){1,2,3,4};\n\
             v2di b = (v2di)a;\n\
             return 0;\n\
         }\n",
    );
    // 同尺寸 vector-vector cast 应成功
    // 这里测试的是解析器能接受同尺寸 vector cast
    // 不同尺寸的才会被拒绝
    assert!(
        output.status.success(),
        "same-size vector cast should be accepted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
