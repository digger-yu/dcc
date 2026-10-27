// included from tests/integration.rs — not a standalone cargo target

#[test]
fn md_dependency_file_lists_includes() {
    let dir = std::env::temp_dir();
    let hdr_name = format!("dcc-md-hdr-{}.h", std::process::id());
    let hdr_path = dir.join(&hdr_name);
    let source_path = dir.join(format!("dcc-md-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-md-{}.o", std::process::id()));
    let dep_path = dir.join(format!("dcc-md-{}.d", std::process::id()));
    fs::write(&hdr_path, "int hdr_var;\n").expect("write header");
    fs::write(
        &source_path,
        format!("#include \"{hdr_name}\"\nint main(void) {{ return hdr_var; }}\n"),
    )
    .expect("write source");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-MD", "-MF"])
        .arg(&dep_path)
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep_path).unwrap_or_default();
    let _ = fs::remove_file(&hdr_path);
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&out_path);
    let _ = fs::remove_file(&dep_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(dep_text.contains(&hdr_name), "dep file missing header: {dep_text}");
}

#[test]
fn dep_file_uses_backslash_continuations() {
    let dir = std::env::temp_dir();
    let hdr = format!("dcc-depcont-hdr-{}.h", std::process::id());
    let src_path = dir.join(format!("dcc-depcont-{}.c", std::process::id()));
    let hdr_path = dir.join(&hdr);
    let obj_path = dir.join(format!("dcc-depcont-{}.o", std::process::id()));
    let dep_path = dir.join(format!("dcc-depcont-{}.d", std::process::id()));
    fs::write(&hdr_path, "#define V 1\n").unwrap();
    fs::write(&src_path, format!("#include \"{}\"\nint main(void) {{ return V; }}\n", hdr))
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-MD", "-MF"])
        .arg(&dep_path)
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep_path).unwrap_or_default();
    let _ = fs::remove_file(&hdr_path);
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
    let _ = fs::remove_file(&dep_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // GCC 兼容格式: 所有非末行应以 \ 续行符结尾
    let lines: Vec<&str> = dep_text.lines().collect();
    if lines.len() > 1 {
        for line in &lines[..lines.len() - 1] {
            assert!(line.ends_with('\\'), "dep line should end with \\: {line:?}");
        }
    }
}

#[test]
fn dep_file_mp_emits_phony_targets() {
    let dir = std::env::temp_dir();
    let hdr = format!("dcc-depmp-hdr-{}.h", std::process::id());
    let src_path = dir.join(format!("dcc-depmp-{}.c", std::process::id()));
    let hdr_path = dir.join(&hdr);
    let obj_path = dir.join(format!("dcc-depmp-{}.o", std::process::id()));
    let dep_path = dir.join(format!("dcc-depmp-{}.d", std::process::id()));
    fs::write(&hdr_path, "#define V 2\n").unwrap();
    fs::write(&src_path, format!("#include \"{}\"\nint main(void) {{ return V; }}\n", hdr))
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-MD", "-MP", "-MF"])
        .arg(&dep_path)
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep_path).unwrap_or_default();
    let _ = fs::remove_file(&hdr_path);
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
    let _ = fs::remove_file(&dep_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // -MP: 每个头文件应有 phony 目标 (hdr:)
    assert!(
        dep_text.contains(&format!("{}:", hdr)),
        "-MP should emit phony target for header: {dep_text}"
    );
}

/// TU init 一次 + 每个函数入口 coverage 桩
#[test]
fn sanitizer_prologue_in_all_functions() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-san-multi-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-san-multi-{}.s", std::process::id()));
    let source = "int a(void) { return 1; }\nint b(void) { return 2; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-fsanitize-coverage", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(asm.matches("__asan_init").count(), 1, "TU init once: {asm}");
    assert!(
        asm.matches("__sanitizer_cov_trace_pc").count() >= 2,
        "per-function coverage stub: {asm}"
    );
}

#[test]
#[cfg(unix)]
fn host_wrapper_fixdep_failure_does_not_write_stub_depfile() {
    let script_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/dcc-host-wrapper.sh");
    let temp = std::env::temp_dir().join(format!("dcc-fixdep-failure-{}", std::process::id()));
    fs::create_dir_all(&temp).expect("create fixdep fixture");
    let depfile = temp.join("fixdep.d");
    let output_path = temp.join("fixdep");
    let source_path = temp.join("fixdep.c");
    fs::write(&source_path, "int main(void) { return 0; }\n").expect("write fixdep source");
    let output = Command::new("bash")
        .arg(script_path)
        .args(["-c", "-o"])
        .arg(&output_path)
        .arg(format!("-Wp,-MMD,{}", depfile.display()))
        .arg(&source_path)
        .env("DCC_BIN", "false")
        .output()
        .expect("run dcc-host-wrapper.sh");
    assert!(!output.status.success());
    assert!(!depfile.exists(), "failed fixdep must not create stub depfile");
    let _ = fs::remove_dir_all(&temp);
}

/// #pragma GCC poison: 编译时不触发错误 (未使用被 poison 的标识符)
#[test]
#[cfg(unix)]
fn gcc_poison_compile_and_run() {
    let source = include_str!("../fixtures/c/gcc_poison.c");
    let (output, exe) = run_compile_and_run("gcc-poison", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "gcc_poison test should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// UBSAN div/mod: 除零和取模零均应调用 divrem runtime handler.
#[test]
#[cfg(unix)]
fn ubsan_runtime_division_by_zero() {
    let div_source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_divrem_overflow(void *data, long rhs) {\n\
    (void)data;\n\
    exit(rhs == 0 ? 91 : 90);\n\
}\n\
int f(int x) { return 42 / x; }\n\
int main(void) { return f(0); }\n";
    let (_, div_run, div_log, div_exe) =
        run_sanitized_binary("ubsan-div", div_source, &["-fsanitize=undefined"]);
    assert_exit_code(&div_run, 91, &div_log);
    let _ = fs::remove_file(&div_exe);

    let mod_source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_divrem_overflow(void *data, long rhs) {\n\
    (void)data;\n\
    exit(rhs == 0 ? 96 : 90);\n\
}\n\
int f(int x) { return 42 % x; }\n\
int main(void) { return f(0); }\n";
    let (_, mod_run, mod_log, mod_exe) =
        run_sanitized_binary("ubsan-mod", mod_source, &["-fsanitize=undefined"]);
    assert_exit_code(&mod_run, 96, &mod_log);
    let _ = fs::remove_file(&mod_exe);
}

/// UBSAN shift: 位移溢出应调用 shift runtime handler.
#[test]
#[cfg(unix)]
fn ubsan_runtime_shift_overflow() {
    let source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_shift_out_of_bounds(void *data, long rhs) {\n\
    (void)data;\n\
    exit(rhs == 32 ? 92 : 90);\n\
}\n\
int f(int x) { return 1 << x; }\n\
int main(void) { return f(32); }\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("ubsan-shift", source, &["-fsanitize=undefined"]);
    assert_exit_code(&run, 92, &log);
    let _ = fs::remove_file(&exe_path);
}

/// UBSAN VLA: 非正变长数组边界应调用 VLA runtime handler.
#[test]
#[cfg(unix)]
fn ubsan_runtime_vla() {
    let source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_vla_bound_not_positive(long bound) {\n\
    exit(bound < 1 ? 93 : 90);\n\
}\n\
int zero;\n\
int f(int n) { int arr[n]; return n; }\n\
int main(void) { return f(zero); }\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("ubsan-vla", source, &["-fsanitize=undefined"]);
    assert_exit_code(&run, 93, &log);
    let _ = fs::remove_file(&exe_path);
}

/// UBSAN null: 空指针解引用应调用 type_mismatch runtime handler.
#[test]
#[cfg(unix)]
fn ubsan_runtime_null_dereference() {
    let source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_type_mismatch_v1(void *data) {\n\
    (void)data;\n\
    exit(94);\n\
}\n\
int f(int *p) { return *p; }\n\
int main(void) { return f(0); }\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("ubsan-null", source, &["-fsanitize=undefined"]);
    assert_exit_code(&run, 94, &log);
    let _ = fs::remove_file(&exe_path);
}

/// UBSAN alignment: 对齐违规应调用 type_mismatch_v1 handler.
/// 将 int 地址 +1 强转为 int* 后解引用, 指针不满足 4 字节对齐。
#[test]
#[cfg(unix)]
fn ubsan_runtime_alignment_violation() {
    let source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_type_mismatch_v1(void *data) {\n\
    (void)data;\n\
    exit(98);\n\
}\n\
int buf[4];\n\
int main(void) {\n\
    char *base = (char *)&buf[0];\n\
    int *misaligned = (int *)(base + 1);\n\
    return *misaligned;\n\
}\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("ubsan-align", source, &["-fsanitize=undefined"]);
    assert_exit_code(&run, 98, &log);
    let _ = fs::remove_file(&exe_path);
}

/// UBSAN float-cast: 浮点转换溢出应调用 float-cast runtime handler.
#[test]
#[cfg(unix)]
fn ubsan_runtime_float_cast() {
    let source = "extern void exit(int);\n\
__attribute__((no_sanitize(\"undefined\")))\n\
void __ubsan_handle_float_cast_overflow(float v, int lo, int hi) {\n\
    (void)lo;\n\
    exit(v > hi ? 95 : 90);\n\
}\n\
int f(float x) { return (int)x; }\n\
int main(void) { return f(1e20f); }\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("ubsan-float", source, &["-fsanitize=undefined"]);
    assert_exit_code(&run, 95, &log);
    let _ = fs::remove_file(&exe_path);
}

/// HWASAN/MTE: 软件 tag 路径测试.
/// -fsanitize=hwaddress 时应发射 init 与 HWASAN load/store 回调.
#[test]
#[cfg(unix)]
fn hwasan_software_tag_path() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-hwasan-tag-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-hwasan-tag-{}.s", std::process::id()));
    let source = "int f(int *p) { *p = 7; return *p; }\nint main(void) { int x; return f(&x); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=hwaddress", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile HWASAN tag fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("__hwasan_init"),
        "HWASAN should emit __hwasan_init call: {asm_text}"
    );
    assert!(
        asm_text.contains("__hwasan_store4"),
        "HWASAN should emit store tag checks: {asm_text}"
    );
    assert!(
        asm_text.contains("__hwasan_load4"),
        "HWASAN should emit load tag checks: {asm_text}"
    );
    assert!(
        !asm_text.contains("__asan_load") && !asm_text.contains("__asan_store"),
        "HWASAN path should not use ASAN callbacks: {asm_text}"
    );
}

// ---------- unknown sanitizer ----------

/// unknown sanitizer: 未知 sanitizer/coverage 名称的负向诊断测试.
/// dcc 应输出明确 unsupported sanitizer 诊断并拒绝产生目标文件.
#[test]
fn unknown_sanitizer_negative_diagnostic() {
    fn assert_unknown_flag(flag: &str, expected: &str) {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let source_path = dir.join(format!("dcc-unknown-san-{pid}.c"));
        let obj_path = dir.join(format!("dcc-unknown-san-{pid}.o"));
        let source = "int main(void) { return 0; }\n";
        fs::write(&source_path, source).expect("write C fixture");
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args([flag, "-c", "-o"])
            .arg(&obj_path)
            .arg(&source_path)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let produced_obj = obj_path.exists();
        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(obj_path);
        assert!(!output.status.success(), "{flag} should be rejected");
        assert!(
            stderr.contains(expected),
            "{flag} should produce exact unsupported diagnostic: {stderr}"
        );
        assert!(!produced_obj, "{flag} should not produce an object file");
    }

    assert_unknown_flag(
        "-fsanitize=unknown-sanitizer-name",
        "unsupported sanitizer: unknown-sanitizer-name",
    );
    assert_unknown_flag(
        "-fsanitize-coverage=unknown-coverage-mode",
        "unsupported sanitizer coverage: unknown-coverage-mode",
    );
}

/// -fbounds-check 数组越界检测 (正常路径不触发, 仅编译 + 运行)
#[test]
#[cfg(unix)]
fn m15_bounds_check_inrange() {
    let src = include_str!("../fixtures/c/bounds_check.c");
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-m15-bounds-{pid}.c"));
    let exe = dir.join(format!("dcc-m15-bounds-{pid}"));
    fs::write(&sp, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fbounds-check", "-o"])
        .arg(&exe)
        .arg(&sp)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// -fbounds-check 越界路径: libubsan 诊断 "index 5 out of bounds for type
/// 'int [3]'" (OutOfBoundsData 布局与 gcc libsanitizer 一致: 可写节 +
/// TypeInfo=(log2(bits)<<1)|signed, 元素索引而非字节偏移)
#[test]
#[cfg(unix)]
fn m15_bounds_check_oob_diag() {
    let src = include_str!("../fixtures/c/bounds_check_oob.c");
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-m15-bounds-oob-{pid}.c"));
    let exe = dir.join(format!("dcc-m15-bounds-oob-{pid}"));
    fs::write(&sp, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fbounds-check", "-o"])
        .arg(&exe)
        .arg(&sp)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    // 诊断在运行时的 stderr: 手动执行捕获 (run_exe 只返回码)
    let out = Command::new(&exe).output().expect("run exe");
    let _ = fs::remove_file(&exe);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("index 5 out of bounds for type 'int [3]'"),
        "ubsan diag: {}",
        stderr
    );
}

#[test]
#[cfg(unix)]
fn sanitizer_host_link_adds_runtime_flags() {
    let source = "int main(void) { return 0; }\n";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let source_path = dir.join(format!("dcc-san-hostlink-{pid}.c"));
    let exe_path = dir.join(format!("dcc-san-hostlink-{pid}"));
    fs::write(&source_path, source).expect("write sanitizer fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-v", "-fsanitize=address,undefined", "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("-fsanitize=address") && stderr.contains("-fsanitize=undefined"),
        "host cc link must receive sanitizer runtime flags: {stderr}"
    );
    let run = Command::new(&exe_path).output().expect("run sanitizer exe");
    let _ = fs::remove_file(&exe_path);
    assert!(run.status.success(), "runtime status: {:?}", run.status.code());
}

// ====== M27: Linux FORTIFY __builtin___*_chk 链接行为 ======
/// Linux FORTIFY: __builtin___memcpy_chk 应剥离前缀链接到 __memcpy_chk
#[test]
#[cfg(unix)]
fn m27_fortify_memcpy_chk_links_correctly() {
    let src = include_str!("../fixtures/c/builtin_fortify_chk.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-m27-fortify-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-m27-fortify-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write source");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    // 汇编中应出现 call __memcpy_chk (不带 __builtin_ 前缀)
    assert!(
        asm_text.contains("call __memcpy_chk"),
        "FORTIFY builtin should strip prefix: {}",
        asm_text
    );
}

/// M25 增量: PCH (预编译头) 集成测试. 验证 -fpch-create 与
/// -fpch-include 两条路径能端到端工作:
/// 1. 编译 preamble.c 头引用 + 函数体, 用 -fpch-create 生成 PCH.
/// 2. 编译 main.c 用 -fpch-include 加载 PCH, 验证头宏/typedef
///    仍然可用, 生成的 .o 与不使用 PCH 的对照路径都能正确运行.
/// 3. 对照路径: 不使用 PCH 直接编译 main.c, 同样运行成功.
#[test]
fn pch_create_then_include_compiles_and_runs() {
    let dir = env::temp_dir();
    let header_path = dir.join(format!("dcc-pch-hdr-{}.h", std::process::id()));
    let preamble_src = dir.join(format!("dcc-pch-pre-{}.c", std::process::id()));
    let main_src = dir.join(format!("dcc-pch-main-{}.c", std::process::id()));
    let pch_path = dir.join(format!("dcc-pch-{}.pch", std::process::id()));
    let preamble_o = dir.join(format!("dcc-pch-pre-{}.o", std::process::id()));
    let main_o_pch = dir.join(format!("dcc-pch-main-pch-{}.o", std::process::id()));
    let main_o_nopch = dir.join(format!("dcc-pch-main-nopch-{}.o", std::process::id()));
    let exe_pch = dir.join(format!("dcc-pch-pch-{}", std::process::id()));
    let exe_nopch = dir.join(format!("dcc-pch-nopch-{}", std::process::id()));
    fs::write(
        &header_path,
        b"#define PCH_CONST 42\n#define PCH_INC(x) ((x) + 1)\ntypedef int pch_int;\n",
    )
    .expect("write header");
    let hdr_str = header_path.to_str().unwrap().to_string();
    let preamble_text =
        format!("#include \"{hdr_str}\"\nint get_const(void) {{ return PCH_CONST; }}\n");
    let main_text = format!("#include \"{hdr_str}\"\nint main(void) {{\n    pch_int v = PCH_INC(PCH_CONST);\n    return v == 43 ? 0 : 1;\n}}\n");
    fs::write(&preamble_src, preamble_text.as_bytes()).expect("write preamble");
    fs::write(&main_src, main_text.as_bytes()).expect("write main");
    // Step 1: create PCH from preamble
    let create_out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-create={}", pch_path.display()))
        .args(["-c", "-o"])
        .arg(&preamble_o)
        .arg(&preamble_src)
        .output()
        .expect("run dcc -fpch-create");
    assert!(
        create_out.status.success(),
        "PCH create must succeed; stderr: {}",
        String::from_utf8_lossy(&create_out.stderr)
    );
    assert!(pch_path.exists(), "PCH file must be created");
    // Step 2: compile main with -fpch-include
    let with_pch = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-include={}", pch_path.display()))
        .args(["-c", "-o"])
        .arg(&main_o_pch)
        .arg(&main_src)
        .output()
        .expect("run dcc -fpch-include");
    assert!(
        with_pch.status.success(),
        "PCH include must succeed; stderr: {}",
        String::from_utf8_lossy(&with_pch.stderr)
    );
    // Step 3: link + run with PCH
    let _ = fs::set_permissions(&exe_pch, fs::Permissions::from_mode(0o755));
    let link_pch = Command::new("gcc")
        .arg(&main_o_pch)
        .arg("-o")
        .arg(&exe_pch)
        .output()
        .expect("link pch");
    assert!(
        link_pch.status.success(),
        "link (PCH) failed; stderr: {}",
        String::from_utf8_lossy(&link_pch.stderr)
    );
    let run_pch = Command::new(&exe_pch).status().expect("run pch");
    assert_eq!(run_pch.code(), Some(0), "PCH build must return 0");
    // Step 4: baseline (no PCH) for parity check
    let nopch = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&main_o_nopch)
        .arg(&main_src)
        .output()
        .expect("run dcc (no pch)");
    assert!(
        nopch.status.success(),
        "no-PCH compile must succeed; stderr: {}",
        String::from_utf8_lossy(&nopch.stderr)
    );
    let _ = fs::set_permissions(&exe_nopch, fs::Permissions::from_mode(0o755));
    let link_nopch = Command::new("gcc")
        .arg(&main_o_nopch)
        .arg("-o")
        .arg(&exe_nopch)
        .output()
        .expect("link nopch");
    assert!(
        link_nopch.status.success(),
        "link (no PCH) failed; stderr: {}",
        String::from_utf8_lossy(&link_nopch.stderr)
    );
    let run_nopch = Command::new(&exe_nopch).status().expect("run nopch");
    assert_eq!(run_nopch.code(), Some(0), "no-PCH build must return 0");
    // cleanup
    let _ = fs::remove_file(&header_path);
    let _ = fs::remove_file(&preamble_src);
    let _ = fs::remove_file(&main_src);
    let _ = fs::remove_file(&pch_path);
    let _ = fs::remove_file(&preamble_o);
    let _ = fs::remove_file(&main_o_pch);
    let _ = fs::remove_file(&main_o_nopch);
    let _ = fs::remove_file(&exe_pch);
    let _ = fs::remove_file(&exe_nopch);
}

/// PCH 降级: 加载失败应仅输出 warning, 并按普通无 PCH 路径继续。
#[test]
fn pch_missing_include_warns_and_compiles() {

    let dir = env::temp_dir();
    let source_path = dir.join(format!("dcc-pch-missing-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-pch-missing-{}.o", std::process::id()));
    let missing_pch = dir.join(format!("dcc-pch-missing-{}.pch", std::process::id()));
    fs::write(&source_path, b"int main(void) { return 0; }\n").expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-include={}", missing_pch.display()))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc -fpch-include missing");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "missing PCH must not fail; stderr: {}", stderr);
    assert!(stderr.contains("W0401"), "stderr: {stderr}");
    assert!(object_path.exists(), "object should be produced");

    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&object_path);
}

/// PCH 降级: 写入失败应仅输出 warning, 编译产物仍正常生成。
#[test]
fn pch_create_write_failure_warns_and_compiles() {

    let dir = env::temp_dir();
    let source_path = dir.join(format!("dcc-pch-writefail-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-pch-writefail-{}.o", std::process::id()));
    let pch_path = dir
        .join(format!("dcc-pch-writefail-missing-{}", std::process::id()))
        .join("out.pch");
    fs::write(&source_path, b"#define A 1\nint x = A;\n").expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-create={}", pch_path.display()))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc -fpch-create write failure");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "PCH write failure must not fail; stderr: {}", stderr);
    assert!(stderr.contains("W0402"), "stderr: {stderr}");
    assert!(object_path.exists(), "object should be produced");
    assert!(!pch_path.exists(), "PCH file should not be created");

    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&object_path);
}

/// PCH 降级: 预处理错误仍是原错误, 但 PCH 创建跳过以 warning 告知。
#[test]
fn pch_create_preproc_error_warns_without_pch() {

    let dir = env::temp_dir();
    let source_path = dir.join(format!("dcc-pch-preerr-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-pch-preerr-{}.o", std::process::id()));
    let pch_path = dir.join(format!("dcc-pch-preerr-{}.pch", std::process::id()));
    fs::write(&source_path, b"#error broken\nint x;\n").expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-create={}", pch_path.display()))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc -fpch-create with preproc error");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "original preprocessor error should still fail");
    assert!(stderr.contains("E0203"), "stderr: {stderr}");
    assert!(stderr.contains("W0403"), "stderr: {stderr}");
    assert!(!pch_path.exists(), "PCH file should not be created");

    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&object_path);
}

/// P1.2: VLA bound UBSAN trap. 编译带 VLA 的代码, 验证
/// 生成的汇编中插入 `call __ubsan_handle_vla_bound_not_positive`
/// (LLVM compiler-rt 符号名). runtime 时用户需链接 ubsan
/// runtime, dcc 只 emit 调用. 测试只验证编译路径
/// (Section 4.2 P1.2). 注: 用 -S 检查汇编而非 .o 的 strtab,
/// 是因为 -S 路径更直接, 无需等待完整链接即可验证.
#[test]
fn ubsan_vla_bound_check_emitted() {
    let src = "\
int f(int n) {
    int vla[n];
    int sum = 0;
    for (int i = 0; i < n; i++) vla[i] = i;
    for (int i = 0; i < n; i++) sum += vla[i];
    return sum;
}
int main(void) { return f(4); }
";
    let dir = std::env::temp_dir();
    // 文件名加 -asm 后缀: 避免与 ubsan_runtime_vla 的 run_compile_and_run
    // (同名 dcc-ubsan-vla-{pid}.c) 在并行测试时竞争同一文件
    let source_path = dir.join(format!("dcc-ubsan-vla-asm-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-ubsan-vla-asm-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write VLA fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=undefined", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "VLA + ubsan compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    let needle = "call __ubsan_handle_vla_bound_not_positive";
    assert!(
        asm.contains(needle),
        "VLA bound ubsan stub should be called in asm, but `{}` not found in:\n{}",
        needle,
        asm
    );
}

/// P1.3: Null deref UBSAN trap. 编译通过指针解引用的代码,
/// 验证生成的汇编中插入 `call __ubsan_handle_type_mismatch_v1`
/// (LLVM compiler-rt 符号名). runtime 时用户需链接 ubsan
/// runtime, dcc 只 emit 调用. 测试只验证编译路径
/// (Section 4.2 P1.3). 用 -S 检查汇编原因同 P1.2.
#[test]
fn ubsan_null_deref_check_emitted() {
    let src = "\
int f(int *p) { return *p; }
int main(void) { int x = 42; return f(&x); }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-ubsan-null-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-ubsan-null-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write null-deref fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=undefined", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "null deref + ubsan compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    let needle = "call __ubsan_handle_type_mismatch_v1";
    assert!(
        asm.contains(needle),
        "null deref ubsan stub should be called in asm, but `{}` not found in:\n{}",
        needle,
        asm
    );
}

/// P1.4: Float-cast UBSAN trap. 编译 float→int 强制转换的代码,
/// 验证生成的汇编中插入 `call __ubsan_handle_float_cast_overflow`
/// (LLVM compiler-rt 符号名). runtime 时用户需链接 ubsan
/// runtime, dcc 只 emit 调用. 测试只验证编译路径
/// (Section 4.2 P1.4). 用 -S 检查汇编原因同 P1.2.
#[test]
fn ubsan_float_cast_check_emitted() {
    let src = "\
int f(double d) { return (int)d; }
int main(void) { return f(3.14); }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-ubsan-fc-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-ubsan-fc-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write float-cast fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=undefined", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "float cast + ubsan compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    let needle = "call __ubsan_handle_float_cast_overflow";
    assert!(
        asm.contains(needle),
        "float cast ubsan stub should be called in asm, but `{}` not found in:\n{}",
        needle,
        asm
    );
}

/// P1.5: user-mode ThreadSanitizer path. 编译 TSan-only 代码
/// (`-fsanitize=thread`, 不启 address), 验证生成的汇编中插入
/// libtsan-compatible `__tsan_readN`, 而不是走 KASAN runtime。
#[test]
fn tsan_only_emits_tsan_access() {
    let src = "\
int f(int *p) { return *p; }
int main(void) { int x = 42; return f(&x); }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-tsan-only-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-tsan-only-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write tsan fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=thread", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "tsan-only compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    let tsan_needle = "call __tsan_read4";
    assert!(
        asm.contains(tsan_needle),
        "tsan access check should be called in asm, but `{}` not found in:\n{}",
        tsan_needle,
        asm
    );
    // 同时确认 KASAN 路径没有被启用 (没有 __asan_loadN 调用).
    assert!(
        !asm.contains("call __asan_load") && !asm.contains("call __asan_store"),
        "KASAN path must not be emitted when only TSan is enabled:\n{}",
        asm
    );
}

/// M3.10: Stack canary (-fstack-protector) support.
/// Tests canary prologue/epilogue with ET_EXEC inline exit on failure.
#[test]
#[cfg(unix)]
fn m3_10_stack_canary() {
    let src = include_str!("../fixtures/c/stack_canary.c");
    let (out, exe) = run_compile_and_run_flags("m3-10-canary", src, &["-fstack-protector"]);
    assert!(
        out.status.success(),
        "m3_10 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(code, 0, "Stack canary checks failed (exit={code})");
}

/// F2: bitfield RMW temp slots must not overwrite the canary. Built
/// with -fstack-protector=all so every function installs a canary at
/// -8(%r13) — any bitfield RMW write into that slot aborts here.
#[test]
#[cfg(unix)]
fn bitfield_rmw_canary() {
    let src = include_str!("../fixtures/c/bitfield_rmw_canary.c");
    let (out, exe) = run_compile_and_run_flags(
        "bitfield-rmw-canary",
        src,
        &["-fstack-protector=all"],
    );
    assert!(
        out.status.success(),
        "bitfield_rmw_canary compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(code, 0, "bitfield_rmw_canary checks failed (exit={code})");
}

/// PCH 损坏内容 (非合法 PCH 格式): 验证 W0401 warning 回退
#[test]
fn pch_negative_corrupted_content() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-pch-corrupt-src-{}.c", std::process::id()));
    let pch = dir.join(format!("dcc-pch-corrupt-{}.pch", std::process::id()));

    // 写入随机字节作为损坏的 PCH 文件
    fs::write(&pch, b"NOT A VALID PCH FILE CONTENT\x00\x01\x02").expect("write corrupted pch");
    fs::write(&src, "int x = 1;\n").expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-include={}", pch.display()))
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-pch-corrupt-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run PCH include with corrupted file");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&pch);
    let _ = fs::remove_file(dir.join(format!("dcc-pch-corrupt-{}.o", std::process::id())));

    assert!(out.status.success(), "corrupted PCH must not be fatal; stderr: {stderr}");
    assert!(stderr.contains("W0401"), "expected W0401 for corrupted PCH: {stderr}");
}

/// PCH 写入失败 snapshot: 父目录不存在时写 PCH 应发 warning 但不阻塞编译
#[test]
fn pch_negative_write_failure_snapshot() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-pch-wf-src-{}.c", std::process::id()));
    // 路径深层嵌套导致父目录不存在
    let pch = dir
        .join(format!("dcc-pch-wf-{}", std::process::id()))
        .join("missing")
        .join("deep")
        .join("out.pch");

    fs::write(&src, "#define A 1\nint x = A;\n").expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-create={}", pch.display()))
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-pch-wf-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run PCH create with write failure");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(dir.join(format!("dcc-pch-wf-{}.o", std::process::id())));

    assert!(out.status.success(), "PCH write failure must not be fatal; stderr: {stderr}");
    assert!(stderr.contains("W0402"), "expected W0402 for write failure: {stderr}");
    assert!(!pch.exists(), "PCH file should not be created on write failure");
}

// ============================================================================
// depfile/fixdep tests (spaces, backslashes, escaped paths, -MP phony)
// ============================================================================

/// fixdep 反解析: 路径含空格 (GCC 转义格式) 应正确还原
#[test]
fn fixdep_unescape_paths_with_spaces() {
    let temp = std::env::temp_dir().join(format!("dcc-fixdep-space-{}", std::process::id()));
    fs::create_dir_all(&temp).expect("create temp");

    let depfile = temp.join("test.d");
    // GCC 格式: 路径中的空格用 \ 转义
    let content = "my\\ obj.o: my\\ src.c \\\n  my\\ header.h \\\n  path\\ with\\ space.h\n";
    fs::write(&depfile, content).expect("write depfile");

    let mut out = Vec::new();
    dcc::fixdep::parse_dep_file(content, "my obj.o", &mut out).unwrap();
    let result = String::from_utf8(out).unwrap();

    let _ = fs::remove_dir_all(&temp);

    // 反转义后应包含原始路径
    assert!(result.contains("my src.c"), "result: {result}");
    assert!(result.contains("my header.h"), "result: {result}");
    assert!(result.contains("path with space.h"), "result: {result}");
}

/// fixdep 反解析: -MP phony 目标应被正确跳过 (不视为源文件)
#[test]
fn fixdep_skips_mp_phony_targets() {
    let temp = std::env::temp_dir().join(format!("dcc-fixdep-mp-{}", std::process::id()));
    fs::create_dir_all(&temp).expect("create temp");

    let depfile = temp.join("test.d");
    // GCC -MP 会生成 phony target 条目
    let content = "foo.o: foo.c bar.h \\\n  baz.h\n\nbar.h:\nbaz.h:\n";
    fs::write(&depfile, content).expect("write depfile");

    let mut out = Vec::new();
    dcc::fixdep::parse_dep_file(content, "foo.o", &mut out).unwrap();
    let result = String::from_utf8(out).unwrap();

    let _ = fs::remove_dir_all(&temp);

    // phony target (bar.h:, baz.h:) 不应出现在 deps 中
    assert!(result.contains("foo.c"), "result: {result}");
    assert!(result.contains("bar.h"), "result: {result}");
    assert!(result.contains("baz.h"), "result: {result}");
    // phony 条目本身不应产生额外的 source_ 前缀
    let source_count = result.matches("source_").count();
    assert_eq!(source_count, 1, "should have exactly one source entry: {result}");
}

/// depfile 反斜杠续行 + 转义路径综合测试
#[test]
fn depfile_backslash_continuation_with_escaped_paths() {

    let dir = std::env::temp_dir();
    // 创建含空格的目录和文件
    let inc = dir.join(format!("dcc-dep-inc-{}", std::process::id()));
    fs::create_dir_all(&inc).expect("mkdir inc");

    let src = dir.join(format!("dcc-dep-src-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-dep-src-{}.o", std::process::id()));
    let dep = dir.join(format!("dcc-dep-src-{}.d", std::process::id()));

    // 头文件名含特殊字符
    let hdr = inc.join("my header.h");
    fs::write(&hdr, "#define VAL 42\n").expect("write header");
    fs::write(&src, "#include \"my header.h\"\nint main(void) { return VAL; }\n")
        .expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-MMD", "-MF"])
        .arg(&dep)
        .arg("-I")
        .arg(&inc)
        .arg("-o")
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc -MMD");

    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    let _ = fs::remove_dir_all(&inc);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&dep);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "depfile with escaped paths must succeed; stderr: {stderr}"
    );
    // depfile 应追踪头文件
    assert!(dep_text.contains("header.h"), "depfile should track header: {dep_text}");
}

/// Feature 9: Stack guard register customization (-mstack-protector-guard-reg=gs)
#[test]
#[cfg(unix)]
fn stack_guard_reg_gs() {
    let src = r#"
int func(void) {
    int x = 42;
    return x;
}
"#;
    let tmp = std::env::temp_dir();
    let src_path = tmp.join("stack_guard_gs.c");
    let obj_path = tmp.join("stack_guard_gs.o");
    fs::write(&src_path, src).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-fstack-protector",
            "-mstack-protector-guard-reg=gs",
            "-o",
        ])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -mstack-protector-guard-reg=gs");
    assert!(
        output.status.success(),
        "stack guard gs compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

// ----- Incremental compilation (-fcached) tests -----

#[test]
fn incremental_first_compile_produces_object() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-incr-integ-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    let src = work.join("test.c");
    let obj = work.join("test.o");
    fs::write(&src, "int foo(void) { return 42; }\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");

    assert!(
        output.status.success(),
        "dcc -fcached first compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(obj.exists(), "output .o should exist after first compile");

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn incremental_second_compile_skipped() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-incr-skip-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    let src = work.join("test.c");
    let obj = work.join("test.o");
    fs::write(&src, "int bar(void) { return 7; }\n").unwrap();

    // First compile
    let output1 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc first");
    assert!(output1.status.success());

    // Record object mtime
    let mtime1 = fs::metadata(&obj).unwrap().modified().unwrap();

    // Small delay to ensure mtime would differ if rewritten
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Second compile (should be skipped - cache hit)
    let output2 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc second");
    assert!(output2.status.success());

    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    // Verbose output should show "up to date" or "skipped"
    assert!(
        stderr2.contains("up to date") || stderr2.contains("skipped"),
        "second compile should be skipped (cache hit), stderr: {stderr2}"
    );

    // Object should NOT have been rewritten
    let mtime2 = fs::metadata(&obj).unwrap().modified().unwrap();
    assert_eq!(mtime1, mtime2, "object file should not be rewritten on cache hit");

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn incremental_source_change_triggers_recompile() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-incr-chg-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    let src = work.join("test.c");
    let obj = work.join("test.o");
    fs::write(&src, "int v1(void) { return 1; }\n").unwrap();

    // First compile
    let output1 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc first");
    assert!(output1.status.success());

    // Modify source
    fs::write(&src, "int v2(void) { return 2; }\n").unwrap();

    // Second compile (should recompile - source changed)
    let output2 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc second");
    assert!(output2.status.success());

    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        stderr2.contains("fingerprint changed") || stderr2.contains("recompiling"),
        "should recompile after source change, stderr: {stderr2}"
    );

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn incremental_flag_change_triggers_recompile() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-incr-flag-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    let src = work.join("test.c");
    let obj = work.join("test.o");
    fs::write(&src, "int x(void) { return 0; }\n").unwrap();

    // First compile with -O0
    let output1 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-v", "-O0", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc first");
    assert!(output1.status.success());

    // Second compile with -O2 (flag changed)
    let output2 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fcached", "-v", "-O2", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc second");
    assert!(output2.status.success());

    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        stderr2.contains("fingerprint changed") || stderr2.contains("recompiling"),
        "should recompile after flag change, stderr: {stderr2}"
    );

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn incremental_without_flag_always_compiles() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let work = dir.join(format!("dcc-incr-noflag-{pid}"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    let src = work.join("test.c");
    let obj = work.join("test.o");
    fs::write(&src, "int y(void) { return 3; }\n").unwrap();

    // First compile without -fcached
    let output1 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc first");
    assert!(output1.status.success());
    let stderr1 = String::from_utf8_lossy(&output1.stderr);
    assert!(stderr1.contains("wrote"), "should write output, stderr: {stderr1}");

    // Second compile without -fcached (should always compile)
    let output2 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc second");
    assert!(output2.status.success());
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        stderr2.contains("wrote"),
        "without -fcached should always compile, stderr: {stderr2}"
    );

    let _ = fs::remove_dir_all(&work);
}

#[test]
fn incremental_cli_flag_parsing() {
    // Test -fcached flag
    let args = vec![
        "-fcached".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-fcached should parse");
    assert!(cfg.incremental);

    // Test --incremental flag
    let args = vec![
        "--incremental".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("--incremental should parse");
    assert!(cfg.incremental);

    // Test -fno-cached flag
    let args = vec![
        "-fcached".to_string(),
        "-fno-cached".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-fno-cached should parse");
    assert!(!cfg.incremental);

    // Test default (no flag)
    let args = vec!["-c".to_string(), "input.c".to_string()];
    let cfg = dcc::cli::parse(&args).expect("default should parse");
    assert!(!cfg.incremental);
}

/// HWASAN/MTE: AArch64 irg/addg/stg 汇编断言.
/// 在 AArch64 目标下, -S 输出应保留 irg/addg/stg 指令文本.
/// 若 host 非 AArch64 或 dcc 无 AArch64 后端, 跳过此测试.
#[test]
#[cfg(unix)]
fn hwasan_mte_aarch64_asm_instructions() {
    // dcc 仅在 host 为 AArch64 时有完整后端; 其他架构跳过.
    if std::env::consts::ARCH != "aarch64" {
        eprintln!("SKIP: AArch64 asm test requires AArch64 host");
        return;
    }
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-hwasan-a64-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-hwasan-a64-{}.s", std::process::id()));
    let source = "void f(void) { asm volatile(\"irg x0, x1\"); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["--target", "aarch64-linux-gnu", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile AArch64 asm fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // -S 输出应保留 irg 指令文本
    assert!(
        asm_text.contains("irg"),
        "AArch64 -S output should preserve irg instruction: {asm_text}"
    );
}

/// HWASAN/MTE: AArch64 addg 指令断言.
/// 若 host 非 AArch64, 跳过此测试.
#[test]
#[cfg(unix)]
fn hwasan_mte_aarch64_addg_asm() {
    if std::env::consts::ARCH != "aarch64" {
        eprintln!("SKIP: AArch64 addg test requires AArch64 host");
        return;
    }
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-hwasan-addg-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-hwasan-addg-{}.s", std::process::id()));
    let source = "void f(void) { asm volatile(\"addg x0, x1, #1\"); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["--target", "aarch64-linux-gnu", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile AArch64 addg fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("addg"),
        "AArch64 -S output should preserve addg instruction: {asm_text}"
    );
}

/// HWASAN/MTE: AArch64 stg 指令断言.
/// 若 host 非 AArch64, 跳过此测试.
#[test]
#[cfg(unix)]
fn hwasan_mte_aarch64_stg_asm() {
    if std::env::consts::ARCH != "aarch64" {
        eprintln!("SKIP: AArch64 stg test requires AArch64 host");
        return;
    }
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-hwasan-stg-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-hwasan-stg-{}.s", std::process::id()));
    let source = "void f(void) { asm volatile(\"stg x0, [x1]\"); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["--target", "aarch64-linux-gnu", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile AArch64 stg fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("stg"),
        "AArch64 -S output should preserve stg instruction: {asm_text}"
    );
}

#[test]
#[cfg(unix)]
fn sanitizer_builtin_pie_records_runtime_needed() {
    let source = "int main(void) { int a = 1; int *p = &a; return *p - 1; }\n";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let source_path = dir.join(format!("dcc-san-pie-{pid}.c"));
    let exe_path = dir.join(format!("dcc-san-pie-{pid}"));
    fs::write(&source_path, source).expect("write sanitizer fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-pie", "-fsanitize=address,undefined", "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let exe_data = std::fs::read(&exe_path).expect("read pie output");
    let _ = fs::remove_file(&exe_path);
    let needed = elf64_dynamic_needed(&exe_data);
    assert!(
        needed.iter().any(|n| n == "libasan.so"),
        "PIE sanitizer link should record libasan.so: {needed:?}"
    );
    assert!(
        needed.iter().any(|n| n == "libubsan.so"),
        "PIE sanitizer link should record libubsan.so: {needed:?}"
    );
}

/// PCH hash mismatch with additional -D flag: 验证 W0400 warning 回退
#[test]
fn pch_negative_hash_mismatch_with_define() {

    let dir = std::env::temp_dir();
    let header = dir.join(format!("dcc-pch-neg-hdr-{}.h", std::process::id()));
    let src = dir.join(format!("dcc-pch-neg-src-{}.c", std::process::id()));
    let pch = dir.join(format!("dcc-pch-neg-{}.pch", std::process::id()));

    fs::write(&header, b"#define PCH_VALUE 100\n").expect("write header");

    // 创建 PCH
    let create_src = dir.join(format!("dcc-pch-neg-create-{}.c", std::process::id()));
    fs::write(&create_src, format!("#include \"{}\"\n", header.display()))
        .expect("write create source");
    let create_out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-create={}", pch.display()))
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-pch-neg-create-{}.o", std::process::id())))
        .arg(&create_src)
        .output()
        .expect("run PCH create");
    let _ = fs::remove_file(&create_src);
    let _ = fs::remove_file(dir.join(format!("dcc-pch-neg-create-{}.o", std::process::id())));
    assert!(
        create_out.status.success(),
        "PCH create: {}",
        String::from_utf8_lossy(&create_out.stderr)
    );

    // 用不同 -D 加载 PCH (宏表 hash 不匹配)
    fs::write(&src, "int x = PCH_VALUE;\n").expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-DPCH_VALUE=200"])
        .arg(format!("-fpch-include={}", pch.display()))
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-pch-neg-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run PCH include with mismatch");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&pch);
    let _ = fs::remove_file(dir.join(format!("dcc-pch-neg-{}.o", std::process::id())));

    assert!(out.status.success(), "hash mismatch must not be fatal; stderr: {stderr}");
    assert!(stderr.contains("W0400"), "expected W0400 for hash mismatch: {stderr}");
}

/// PCH 版本不兼容 (模拟旧版本 PCH 魔数): 验证回退路径
#[test]
fn pch_negative_version_incompatibility() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-pch-ver-src-{}.c", std::process::id()));
    let pch = dir.join(format!("dcc-pch-ver-{}.pch", std::process::id()));

    // 模拟旧版本 PCH 魔数 (DCC_PCH_V0 而非 DCC_PCH_V1)
    fs::write(&pch, "DCC_PCH_V0\nSOURCE=test.h\n").expect("write old-version pch");
    fs::write(&src, "int x = 1;\n").expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg(format!("-fpch-include={}", pch.display()))
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-pch-ver-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run PCH include with version mismatch");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&pch);
    let _ = fs::remove_file(dir.join(format!("dcc-pch-ver-{}.o", std::process::id())));

    assert!(out.status.success(), "version mismatch must not be fatal; stderr: {stderr}");
    assert!(stderr.contains("W0401"), "expected W0401 for version mismatch: {stderr}");
}

/// fixdep 反解析含 # 注释的 depfile 应正确跳过注释行
#[test]
fn fixdep_skips_hash_comments_in_depfile() {
    // 注释独占一行, 不含续行; 后续依赖应正常解析
    let content = "foo.o: foo.c bar.h \\\n  # this is a comment\n  baz.h\n";
    let mut out = Vec::new();
    dcc::fixdep::parse_dep_file(content, "foo.o", &mut out).unwrap();
    let result = String::from_utf8(out).unwrap();

    assert!(result.contains("foo.c"), "result: {result}");
    assert!(result.contains("bar.h"), "result: {result}");
    assert!(result.contains("baz.h"), "result: {result}");
}

#[test]
fn m_flag_outputs_deps_to_stdout_includes_system_headers() {
    let dir = std::env::temp_dir();
    let inc = dir.join(format!("dcc-m-inc-{}", std::process::id()));
    let src = dir.join(format!("dcc-m-{}.c", std::process::id()));
    fs::create_dir_all(&inc).expect("mkdir inc");
    fs::write(inc.join("sys.h"), "#define SYS_X 1\n").expect("write sys.h");
    fs::write(&src, "#include <sys.h>\nint main(void) { return SYS_X; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-M")
        .arg("-isystem")
        .arg(&inc)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = fs::remove_dir_all(&inc);
    let _ = fs::remove_file(&src);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // -M 应包含系统头文件
    assert!(stdout.contains("sys.h"), "-M should include system headers: {stdout}");
    // 输出应包含目标名 (默认 stem.o)
    let stem = src.file_stem().unwrap().to_str().unwrap();
    assert!(
        stdout.contains(&format!("{stem}.o:")),
        "-M output should contain target: {stdout}"
    );
}

#[test]
fn mm_flag_outputs_deps_excludes_system_headers() {
    let dir = std::env::temp_dir();
    let inc = dir.join(format!("dcc-mm-inc-{}", std::process::id()));
    let src = dir.join(format!("dcc-mm-{}.c", std::process::id()));
    fs::create_dir_all(&inc).expect("mkdir inc");
    fs::write(inc.join("sys.h"), "#define SYS_X 1\n").expect("write sys.h");
    fs::write(&src, "#include <sys.h>\nint main(void) { return SYS_X; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-MM")
        .arg("-isystem")
        .arg(&inc)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = fs::remove_dir_all(&inc);
    let _ = fs::remove_file(&src);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // -MM 应排除系统头文件
    assert!(!stdout.contains("sys.h"), "-MM should exclude system headers: {stdout}");
}

#[test]
fn mmd_excludes_isystem_headers_from_depfile() {
    let dir = std::env::temp_dir();
    let inc = dir.join(format!("dcc-mmd-inc-{}", std::process::id()));
    let src = dir.join(format!("dcc-mmd-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-mmd-{}.o", std::process::id()));
    let dep = dir.join(format!("dcc-mmd-{}.d", std::process::id()));
    fs::create_dir_all(&inc).expect("mkdir inc");
    fs::write(inc.join("sys.h"), "#define X 1\n").expect("write sys.h");
    fs::write(&src, "#include <sys.h>\nint main(void) { return X; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-MMD", "-MF"])
        .arg(&dep)
        .args(["-isystem"])
        .arg(&inc)
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    let _ = fs::remove_dir_all(inc);
    let _ = fs::remove_file(src);
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(dep);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(!dep_text.contains("sys.h"), "MMD should skip system header: {dep_text}");
}

/// fixdep 文件打开失败应返回非零退出码且不写 stub 输出
#[test]
fn fixdep_file_open_failure_exits_nonzero() {

    let temp = std::env::temp_dir().join(format!("dcc-fixdep-open-{}", std::process::id()));
    fs::create_dir_all(&temp).expect("create temp");

    let nonexistent = temp.join("nonexistent.d");
    let output = Command::new(env!("CARGO_BIN_EXE_fixdep"))
        .arg(&nonexistent)
        .arg("foo.o")
        .arg("cc -c foo.c")
        .output()
        .expect("run fixdep with missing file");

    let _ = fs::remove_dir_all(&temp);

    assert!(!output.status.success(), "fixdep must exit non-zero on missing file");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("error opening file"),
        "stderr should mention open error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mq_flag_quotes_dollar_in_target() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-mq-{}.c", std::process::id()));
    let dep = dir.join(format!("dcc-mq-{}.d", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    // -MQ 应转义 $ 为 $$: 传入 foo$bar.o, dep 文件中应为 foo$$bar.o
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-MD", "-MQ", "foo$bar.o", "-MF"])
        .arg(&dep)
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-mq-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dep);
    let _ = fs::remove_file(dir.join(format!("dcc-mq-{}.o", std::process::id())));
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // -MQ: $ 应被转义为 $$, Make 读取时还原为 $
    assert!(dep_text.contains("foo$$bar.o:"), "-MQ should escape $ to $$: {dep_text}");
}

#[test]
fn m_flag_with_mf_writes_to_file() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-m-mf-{}.c", std::process::id()));
    let dep = dir.join(format!("dcc-m-mf-{}.d", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-M", "-MF"])
        .arg(&dep)
        .arg(&src)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dep);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // -M 加 -MF 应写入文件而非 stdout
    assert!(!dep_text.is_empty(), "-M -MF should write deps to file");
    let stem = src.file_stem().unwrap().to_str().unwrap();
    assert!(
        dep_text.contains(&format!("{stem}.o:")),
        "dep file should contain target: {dep_text}"
    );
}

/// gcov 计数路端到端门 (夹具 `tests/fixtures/c/profile_gcov.c`):
/// 1. `-ftest-coverage -fprofile-arcs -c` 必须产出 `.gcno` + `.gcovrt.c`;
///    `.gcno` 头部 magic 为 'gcno', 且函数名 `square`/`main` 与该源文件名
///    真的出现在里面 (只验证“有插桩元数据”而不只看退出码);
/// 2. 把插桩目标与生成的运行时一起链接后跑起来, 退出码必须等于
///    `square(3)` == 9 —— 证明插桩没改变语义;
/// 3. 运行后落盘的 `.gcda` 必须带 'gcda' magic, 且逐函数 counter 真为
///    `square -> [1, 1]`、`main` 入口块计数 1 ——
///    counter 与 `.gcno` 里的函数集合不一致就在这里报错。
///
/// 写法上沿用 `tests/profile.rs::gcov_end_to_end_counter_gate` 的范式: 编译+产物
/// 断言无条件跑; 链接段需要 host `cc`, 缺失时真实短路 (仅跳过链接段)。
#[test]
#[cfg(unix)]
fn gcov_profile_fixture_emits_gcno_and_counts_blocks() {
    let dir = std::env::temp_dir().join(format!("dcc-gcov-fixture-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create gcov temp dir");
    fs::write(
        dir.join("profile_gcov.c"),
        include_str!("../fixtures/c/profile_gcov.c"),
    )
    .expect("write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .current_dir(&dir)
        .args([
            "-std=gnu23",
            "-ftest-coverage",
            "-fprofile-arcs",
            "-c",
            "profile_gcov.c",
            "-o",
            "profile_gcov.o",
        ])
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "gcov 插桩编译必须成功: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // ---- 1) .gcno / .gcovrt.c 产物与内容 ----
    let gcno = fs::read(dir.join("profile_gcov.gcno")).expect("应产出 .gcno");
    assert!(
        dir.join("profile_gcov.gcovrt.c").exists(),
        "应产出配对的 gcov 运行时 C"
    );
    assert!(dir.join("profile_gcov.o").exists(), "应产出插桩目标文件");
    assert!(gcno.len() > 24, ".gcno 至少应有头部 + 记录, 实际 {}", gcno.len());
    assert_eq!(
        &gcno[0..4],
        &[0x6f, 0x6e, 0x63, 0x67],
        ".gcno magic 应为 'gcno'"
    );
    let gcno_text = String::from_utf8_lossy(&gcno).into_owned();
    for want in ["profile_gcov.c", "square", "main"] {
        assert!(
            gcno_text.contains(want),
            ".gcno 应记录 {want}, 实际内容: {gcno_text:?}"
        );
    }

    if !tool_exists("cc") {
        eprintln!("skip: 无 host cc, 跳过 .gcda 计数断言 (.gcno 侧已断言)");
        let _ = fs::remove_dir_all(&dir);
        return;
    }

    // ---- 2) 链接运行时并运行: 语义不变, 退出码 == square(3) ----
    let link = Command::new("cc")
        .current_dir(&dir)
        .args([
            "-O0",
            "profile_gcov.o",
            "profile_gcov.gcovrt.c",
            "-o",
            "profile_gcov_bin",
        ])
        .output()
        .expect("run cc");
    assert!(
        link.status.success(),
        "链接 gcov 运行时必须成功: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new("./profile_gcov_bin")
        .current_dir(&dir)
        .output()
        .expect("run instrumented binary");
    assert_eq!(
        run.status.code(),
        Some(9),
        "插桩后返回值应仍为 square(3)==9, stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // ---- 3) 解析 .gcda 的逐函数 counter ----
    let gcda = fs::read(dir.join("profile_gcov.gcda")).expect("运行后应产出 .gcda");
    assert_eq!(
        &gcda[0..4],
        &[0x61, 0x64, 0x63, 0x67],
        ".gcda magic 应为 'gcda'"
    );
    assert!(gcda.len() > 24, ".gcda 太短: {}", gcda.len());
    // 布局: magic/version/stamp(12) + runs/sums(8) + source(pascal) + 函数记录
    let mut p = 20usize;
    let slen = u32::from_le_bytes(gcda[p..p + 4].try_into().unwrap()) as usize;
    p += 4 + slen;
    let mut counters: Vec<(String, Vec<u64>)> = Vec::new();
    while p + 16 <= gcda.len() {
        p += 4; // ident
        p += 4; // lineno
        p += 4; // cfg size
        let nlen = u32::from_le_bytes(gcda[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        if p + nlen + 12 > gcda.len() {
            break;
        }
        let name = String::from_utf8_lossy(&gcda[p..p + nlen]).into_owned();
        p += nlen;
        p += 4; // name tag
        p += 4; // counter tag
        let nc = u32::from_le_bytes(gcda[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        if p + 8 * nc > gcda.len() {
            break;
        }
        let mut cs = Vec::with_capacity(nc);
        for _ in 0..nc {
            cs.push(u64::from_le_bytes(gcda[p..p + 8].try_into().unwrap()));
            p += 8;
        }
        counters.push((name, cs));
    }
    let lookup = |want: &str| -> Vec<u64> {
        counters
            .iter()
            .find(|(n, _)| n == want)
            .map(|(_, cs)| cs.clone())
            .unwrap_or_else(|| panic!(".gcda 缺少函数 {want}: {counters:?}"))
    };
    assert_eq!(
        counters.len(),
        2,
        "应记录 square/main 两个函数的 counter, 实际: {counters:?}"
    );
    assert_eq!(
        lookup("square"),
        vec![1u64, 1u64],
        "square 只被调用一次: 入口/返回块各应计数 1"
    );
    assert_eq!(
        lookup("main")[0],
        1,
        "main 入口块应恰好执行一次: {counters:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn mt_flag_uses_literal_target() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-mt-{}.c", std::process::id()));
    let dep = dir.join(format!("dcc-mt-{}.d", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-MD", "-MT", "custom_target.o", "-MF"])
        .arg(&dep)
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-mt-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run dcc");
    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dep);
    let _ = fs::remove_file(dir.join(format!("dcc-mt-{}.o", std::process::id())));
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        dep_text.contains("custom_target.o:"),
        "-MT target should appear literally: {dep_text}"
    );
}

/// sanitizer 插桩包住的 LHS 必须在帧规划期也被认成「聚合成员赋值」。
///
/// 缺陷形状：ASan/KASAN 把 `pi->fg = po->fg` 的 LHS 改写成
/// `Comma{ __asan_load8(&pi->fg), pi->fg }`。发射点先剥壳再分派，而规划期的
/// `agg_member_scratch_used` 直接拿原形状判「不是 Member」，于是那 16 字节的
/// `<agg_member_tmp>` 没进晚分配预算 ⇒ `E1133: late frame allocation ... 2 slots
/// used, 0 reserved`。实测这一条直接挡住整颗 SQLite amalgamation 的
/// `-fsanitize=address` 构建（`sqlite3ExprListDup` / `sqlite3SrcListDup`）。
#[test]
#[cfg(unix)]
fn sanitizer_wrapped_lvalue_keeps_late_frame_budget() {
    let source = "\
struct Item { int n; struct { unsigned f; unsigned g; } fg; };
struct List { int nExpr; struct Item a[4]; };
long dup(struct List *p, struct List *q){
  struct Item *pi; const struct Item *po; int i;
  q->nExpr = p->nExpr; pi = q->a; po = p->a;
  for(i=0;i<p->nExpr;i++,pi++,po++){ pi->n = po->n; pi->fg = po->fg; }
  return q->a[0].fg.f;
}
";
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-san-lhs-{}.c", std::process::id()));
    let op = dir.join(format!("dcc-san-lhs-{}.o", std::process::id()));
    fs::write(&sp, source).expect("write fixture");
    for extra in [
        vec!["-fsanitize=address"],
        vec!["-fsanitize=address", "-O2"],
        vec!["-fsanitize=kernel-address"],
        vec!["-fsanitize=address", "-fsanitize=kernel-address"],
    ] {
        let mut args: Vec<&str> = vec!["-O0", "-c", "-o"];
        args.push(op.to_str().unwrap());
        args.extend(extra.iter().copied());
        args.push(sp.to_str().unwrap());
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(&args)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "sanitizer 插桩不得打爆帧预算（{extra:?}）: {stderr}"
        );
        assert!(
            !stderr.contains("E1133"),
            "不得出现晚分配预算超出（{extra:?}）: {stderr}"
        );
    }
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
}

/// 序言的 poison/unpoison 插桩不得踩掉尚未落栈的入参寄存器（运行期判据）。
///
/// 缺陷形状：`__asan_poison_memory_region` / `__asan_unpoison_memory_region` 是真调用，
/// 而参数落栈发生在函数体开头（比插桩晚）；不屏蔽 rdi/rsi/rdx/rcx/r8/r9 与 xmm0-7 的话，
/// 被调方（libasan）一踩参数就变垃圾 —— 实测 ASan 下 `sqlite3_snprintf` 的指针参数成了
/// `0x1`，第一个 `%s` 就 SEGV（整颗 SQLite amalgamation 因此根本进不到被测路径）。
#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn asan_prologue_poison_preserves_incoming_args() {
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-asan-args-{}.c", std::process::id()));
    let exe = dir.join(format!("dcc-asan-args-{}", std::process::id()));
    let source = "\
#include <stdio.h>\n\
#include <stdarg.h>\n\
#include <string.h>\n\
static int sink;\n\
long fmt(char *out, int n, const char *tag, int a, int b, int c, ...) {\n\
  char local[32];\n\
  va_list ap;\n\
  va_start(ap, c);\n\
  int d = va_arg(ap, int);\n\
  va_end(ap);\n\
  snprintf(out, n, \"%s|%d,%d,%d,%d\", tag, a, b, c, d);\n\
  memcpy(local, tag, strlen(tag) + 1);\n\
  sink = local[0];\n\
  return (long)strlen(out);\n\
}\n\
int main(void) {\n\
  char buf[64];\n\
  fmt(buf, sizeof buf, \"TAG\", 11, 22, 33, 44);\n\
  printf(\"%s\\n\", buf);\n\
  return strcmp(buf, \"TAG|11,22,33,44\") == 0 ? 0 : 1;\n\
}\n";
    fs::write(&sp, source).expect("write fixture");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O0", "-fsanitize=address", "-c", "-o"])
        .arg(dir.join(format!("dcc-asan-args-{}.o", std::process::id())))
        .arg(&sp)
        .output()
        .expect("compile with asan");
    assert!(
        compile.status.success(),
        "ASan 编译失败: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let link = Command::new("gcc")
        .args(["-fsanitize=address", "-no-pie", "-o"])
        .arg(&exe)
        .arg(dir.join(format!("dcc-asan-args-{}.o", std::process::id())))
        .output()
        .expect("link");
    assert!(
        link.status.success(),
        "链接失败: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&exe).output().expect("run");
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(dir.join(format!("dcc-asan-args-{}.o", std::process::id())));
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "ASan 构建里入参被插桩踩坏（崩溃或非零退出）: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "TAG|11,22,33,44",
        "变参函数在 ASan 下必须拿到完整入参（stderr={}）",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// ASan 不得把**自己框架里的活内存**当成红区（运行期判据）。
///
/// 缺陷形状（两重）：旧插桩在序言里毒整个帧，再只解除 `var_map` 里具名变量的毒；
/// 发射期才登记的晚分配带（`<agg_member_tmp>` 等）不在那里，于是终身带毒。改成
/// 「只毒 `__kasan_rz_*` 条目」后又撞上第二重：那个条目的算术与排布推进不同步，
/// 实测 `struct S s; char b[8];` 里红区条目正好落在 `b` 的槽上 —— libc 的 `memcpy`
/// 正常写入被报 `use-after-poison`（同一源码 gcc 的 ASan 构建干净）。最终判据是几何
/// 事实：与任何活区间相交的红区条目一律不毒。本用例钉住这个结论。
#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn asan_stack_poison_does_not_flag_live_slots() {
    let source = "\
#include <stdio.h>
#include <string.h>
struct S { int n; struct { unsigned f, g; } fg; };
int main(void){
  struct S s; char b[8]; char *p = b;
  s.fg.f = 7; s.fg.g = 8; s.n = 9;
  /* 让未插桩的 libc 往帧内活对象里写：旧模型下这里报 use-after-poison */
  memcpy(b, \"abcd\", 5);
  memset(p + 5, 'z', 2);
  if (s.fg.f != 7 || s.fg.g != 8 || s.n != 9) { return 1; }
  if (b[0] != 'a' || b[3] != 'd' || b[5] != 'z' || b[6] != 'z' || b[7] != 0) { return 2; }
  printf(\"ok\\n\");
  return 0;
}
";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-asan-live-{pid}.c"));
    let op = dir.join(format!("dcc-asan-live-{pid}.o"));
    let exe = dir.join(format!("dcc-asan-live-{pid}"));
    fs::write(&sp, source).expect("write fixture");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O0", "-g", "-fsanitize=address", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("compile");
    assert!(
        compile.status.success(),
        "ASan 编译失败: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let link = Command::new("gcc")
        .args(["-fsanitize=address", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&op)
        .output()
        .expect("link");
    assert!(
        link.status.success(),
        "链接失败: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&exe).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr).to_string();
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
    let _ = fs::remove_file(&exe);
    assert!(
        !stderr.contains("AddressSanitizer"),
        "不得把自己的帧内活对象报成红区: {stderr}"
    );
    assert!(
        run.status.success(),
        "ASan 构建里局部存/取错值或崩溃: stderr={stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "ok",
        "成员赋值与 memcpy/memset 必须写入同一位（stderr={stderr}）"
    );
}

/// ASan 下「成员写入的目标地址」必须真算出来，不能取上一条指令的残值。
///
/// 现象（2026-10-27 重新收窄，三条独立最小复现都已量过）：`-O0 -fsanitize=address` 下
/// 连 **一维静态数组的一层成员写** 都错 —— `static struct S A[4]; A[0].f = 10;`
/// 运行期写空指针（写地址成了 0）；把 `f` 换成成员里的 `int n` 则不崩但**读回 4211512**
/// （写飘到别处）。同一份源码 `-O0` 无插桩、`-O2 -fsanitize=address`、gcc 的 ASan 构建
/// 全对；`static struct I g1; g1.fg.f = 10;`（非数组）也对 —— 所以触发条件是
/// **「数组元素」作成员基座**，与循环/嵌套层数/是否聚合 RHS 无关（原先钉的
/// `pi->fg = (probe(), po->fg)` 只是它的一个特例）。
///
/// 读数（`dcc -O0 -fsanitize=address -S` 对 `A[0].f = 10`）：插桩把基座 `A[0]` 包成
/// `Comma{ __asan_load8(&A[0]), A[0] }`（**基座被当成一次 8 字节读**），随后写 `.f`
/// 的那一步取基座地址时没有看穿这层 Comma，而是按**右值**约定取值：
///   `lea A(%rip),%rax; movq (%rax),%rax; mov %rax,%rcx; movl %eax,(%rcx)`
/// 无插桩的同一条是 `lea A(%rip),%rax; mov %rax,%rcx; movl %eax,(%rcx)`。即「该取
/// 地址的地方取了值」——多出来的一次 `movq (%rax)` 就是错处。
///
/// 修它需要同时动三处（都已改）：
///   1. `sanitizer.rs::instrument_store_target` —— 赋值目标的**顶层**访存是写，已由
///      store 检查负责，不得再套一层 load 检查（gcc 对 `A[0].f = x` 只发一笔
///      `__asan_store4(&A[0].f)`）；否则 store 检查的入参就成了 `AddrOf{Comma{…}}`。
///   2. `dispatch.rs` 的 `Expr::AddrOf` 补兜底 —— 旧写法对认不出的形状**什么都不发**，
///      `%rax` 里的残值直接被当成地址交给 `__asan_store4`（「静默不发音」比报错难查）。
///   3. `mem/addr.rs::gen_lvalue_addr` 看穿 `Comma` —— 基座被包成逗号后，点成员取址
///      会退回右值约定，凭空多一次 `movq (%rax)`。
///
/// 同形状的最小复现已缩到三行：`static struct S A[4]; A[0].f = 10;` 在
/// `-O0 -fsanitize=address` 下写空指针。本用例（聚合 RHS 那一条）与它一起构成判据。
#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn asan_aggregate_member_assign_store_addr() {
    // 预期值：a[0].fg.f = 10，a[1].fg.g = 101，probe 被调用 2 次 ⇒ 10 + 101 + 2 = 113
    let source = "\
#include <stdio.h>
struct Item { int n; struct { unsigned f; unsigned g; } fg; };
struct List { int nExpr; struct Item a[4]; };
static int probe_cnt;
static int probe(void){ return ++probe_cnt; }
static long dup(struct List *p, struct List *q){
  struct Item *pi; const struct Item *po; int i;
  q->nExpr = p->nExpr; pi = q->a; po = p->a;
  /* RHS 是逗号表达式 ⇒ 发射点走「一般 RHS」那臂，必经 `<agg_member_tmp>` */
  for(i=0;i<p->nExpr;i++,pi++,po++){ pi->n = po->n; pi->fg = (probe(), po->fg); }
  return (long)(q->a[0].fg.f + q->a[1].fg.g + probe_cnt);
}
int main(void){
  struct List A, B; int i; long got;
  A.nExpr = 2;
  for(i=0;i<2;i++){ A.a[i].n = i+1; A.a[i].fg.f = (unsigned)(10+i); A.a[i].fg.g = (unsigned)(100+i); }
  got = dup(&A, &B);
  printf(\"got=%ld\\n\", got);
  return got == 113 ? 0 : 1;
}
";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-asan-agg-{pid}.c"));
    let op = dir.join(format!("dcc-asan-agg-{pid}.o"));
    let exe = dir.join(format!("dcc-asan-agg-{pid}"));
    fs::write(&sp, source).expect("write fixture");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O0", "-g", "-fsanitize=address", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("compile");
    assert!(
        compile.status.success(),
        "ASan 编译失败: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let link = Command::new("gcc")
        .args(["-fsanitize=address", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&op)
        .output()
        .expect("link");
    assert!(link.status.success(), "链接失败: {}", String::from_utf8_lossy(&link.stderr));
    let run = Command::new(&exe).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr).to_string();
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success() && !stderr.contains("AddressSanitizer"),
        "聚合成员拷贝不得崩溃或报假阳性: stderr={stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "got=113",
        "聚合成员拷贝必须搬对内容（stderr={stderr}）"
    );
}

/// 同一条缺陷的三行最小形状：「数组元素作成员基座」的标量写入。
///
/// 修前：`-O0 -fsanitize=address` 下 `A[0].f = 10` 把 10 写到残值地址（实测写空指针）；
/// 去掉插桩、换 `-O2`、或把数组换成非数组对象都对，gcc 的 ASan 构建也对。
#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn asan_array_element_member_store_addr() {
    let source = "\
struct S { unsigned f; unsigned g; };
static struct S A[4];
static struct S B;
unsigned sink;
int main(void){
  A[0].f = 10u;
  A[1].g = 20u;
  B.f = 30u;
  sink = A[0].f + A[1].g + B.f;
  return sink == 60u ? 0 : 1;
}
";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-asan-arrbase-{pid}.c"));
    let op = dir.join(format!("dcc-asan-arrbase-{pid}.o"));
    let exe = dir.join(format!("dcc-asan-arrbase-{pid}"));
    fs::write(&sp, source).expect("write fixture");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O0", "-g", "-fsanitize=address", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("compile");
    assert!(
        compile.status.success(),
        "ASan 编译失败: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let link = Command::new("gcc")
        .args(["-fsanitize=address", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&op)
        .output()
        .expect("link");
    assert!(
        link.status.success(),
        "链接失败: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&exe).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr).to_string();
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success() && !stderr.contains("AddressSanitizer"),
        "数组元素的成员写入不得崩溃或报假阳性: stderr={stderr}"
    );
}

/// `dcc` 自己链接多输入时也必须透传 `-fsanitize=*`。
///
/// 旧缺口：单 .o 那条链接路径有 `sanitizer_flags` 参数，多文件那条（
/// `link_objects_with_host_cc`）没有——两条各写一遍时漂了。于是
/// `dcc -fsanitize=address a.o b.o -o exe` 不给宿主 cc 传 `-fsanitize=address`，
/// 直接 `undefined reference to __asan_load8/__asan_store4`。本仓常态是「逐 TU 编译
/// 后一次链接」，而现有 ASan 用例都用宿主 `gcc -fsanitize=address` 链接 ⇒ 正好盖住了
/// 这条路径，所以判据必须走 dcc 自己。
#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn dcc_link_forwards_sanitizer_flags_multi_input() {
    let source = "\
#include <stdio.h>
struct L { int n; };
static int sink;
static void bump(struct L *p){ sink += p->n; }
int main(void){ struct L a; a.n = 6; bump(&a); printf(\"%d\\n\", sink); return sink == 6 ? 0 : 1; }
";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-asan-linkfwd-{pid}.c"));
    let op = dir.join(format!("dcc-asan-linkfwd-{pid}.o"));
    let exe = dir.join(format!("dcc-asan-linkfwd-{pid}"));
    fs::write(&sp, source).expect("write fixture");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O0", "-g", "-fsanitize=address", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("compile");
    assert!(
        compile.status.success(),
        "ASan 编译失败: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    // 关键一步：用 **dcc 自己**把 .o 链成可执行（多输入路径）
    let link = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-o"])
        .arg(&exe)
        .arg(&op)
        .output()
        .expect("link");
    let link_err = String::from_utf8_lossy(&link.stderr).to_string();
    assert!(
        link.status.success(),
        "dcc 链接 ASan 产物失败（sanitize 标志没透传给宿主 cc？）: {link_err}"
    );
    assert!(
        !link_err.contains("undefined reference"),
        "链接报 undefined reference，说明没链上 asan runtime: {link_err}"
    );
    let run = Command::new(&exe).output().expect("run");
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "ASan 产物运行失败: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "6");
}
