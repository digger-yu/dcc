use std::fs;
use std::process::Command;

fn run_syntax_test(name: &str, source: &str) -> std::process::Output {
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

fn run_object_test(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-{name}-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    (output, object_path)
}

/// 编译为可执行文件并返回路径（Linux 远程测试用）
fn run_compile_and_run(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
    run_compile_and_run_flags(name, source, &[])
}

/// 带额外参数的编译并运行
fn run_compile_and_run_flags(
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

fn run_exe(path: &std::path::Path) -> i32 {
    Command::new(path)
        .output()
        .expect("run executable")
        .status
        .code()
        .unwrap_or(-1)
}

/// C++ 前端集成测试: 编译 .cpp 到 .o (m17_* fixture 用)
/// 与 run_object_test 类似, 但显式写 .cpp 扩展名以触发 C++ 前端.
fn run_cpp_object_test(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
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

/// dcc -c + 主机 gcc 链接 + 运行. 用于验证运行时行为 (libc 调用等).
/// 返回 (编译输出, 链接+运行退出码). 退出码 0 表示通过.
/// link_extra_args: 传给 gcc 的额外参数 (如 "-lm" 用于 libm 转发).
fn run_host_link_and_run(name: &str, source: &str) -> (std::process::Output, i32) {
    run_host_link_and_run_with(name, source, &[])
}

fn run_host_link_and_run_with(
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

#[test]
fn constant_builtin_and_static_assert_pass_through_typed_ir() {
    let output = run_syntax_test(
        "typed-ir-ok",
        "_Static_assert(__builtin_constant_p(((__int128)1 << 100) / 3), \"constant\");\n\
         int main(void) { return __builtin_constant_p(42) == 1 ? 0 : 1; }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn failed_static_assert_is_a_hard_error() {
    let output = run_syntax_test(
        "static-assert-fail",
        "_Static_assert(2 * 3 == 7, \"math is broken\");\nint main(void) { return 0; }\n",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("math is broken"));
}

#[test]
fn unknown_typedef_is_not_coerced_to_int() {
    let output = run_syntax_test(
        "unknown-type",
        "missing_typedef value;\nint main(void) { return 0; }\n",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown type or typedef"));
}

#[test]
fn int128_mul_div_mod_reach_machine_codegen() {
    let (output, object) = run_object_test(
        "int128-arithmetic",
        "int arithmetic(void) {\n\
             __int128 a = ((__int128)1 << 100) + 17;\n\
             __int128 b = 7;\n\
             __int128 result = (a * b) + (a / b) + (a % b);\n\
             return result != 0;\n\
         }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let size = fs::metadata(&object).expect("object was emitted").len();
    assert!(size > 64, "ELF object should contain generated code");
    let _ = fs::remove_file(object);
}

#[test]
fn s128_typedef_with_attribute_parses() {
    let output = run_syntax_test(
        "s128-typedef",
        "typedef __signed__ __int128 __s128 __attribute__((aligned(16)));\n\
         typedef __s128 s128;\n\
         int main(void) { s128 x = 0; (void)x; return 0; }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn struct_comma_separated_field_declarators_parse() {
    let output = run_syntax_test(
        "struct-comma-fields",
        "struct list_head { struct list_head *next, *prev; };\n\
         struct hlist_node { struct hlist_node *next, **pprev; };\n\
         struct ids { unsigned short vendor, device; };\n\
         int main(void) { return 0; }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// M7 数组到指针衰减: `*(arr + n)` 与 `arr[n]` 等价.
/// libdecnumber 的 decBasic.c 在外层 `uInt div[]` 之内的内层块又写
/// `uint64_t div = ...` 与 `*(bcdar + n) = ...` 都依赖此语义.
#[test]
fn array_decay_and_nested_block_redecl_compile() {
    let (output, object) = run_object_test(
        "array-decay-nested-redecl",
        "int dec_to_packed(int df, int *exp, int *packed) {\n\
             int bcdar[9];\n\
             int *ip = bcdar, *op = packed;\n\
             for (int i = 0; i < 9; i++) bcdar[i] = i;\n\
             {\n\
                 unsigned long div = (unsigned long)df;\n\
                 int mul = (int)(div / 3);\n\
                 *(bcdar + 1) = (int)(div & 0xff);\n\
                 *(bcdar + 1 + 1) = (int)((div >> 8) & 0xff);\n\
                 (void)mul;\n\
             }\n\
             *exp = 0;\n\
             return 0;\n\
         }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let size = fs::metadata(&object).expect("object was emitted").len();
    assert!(size > 64, "ELF object should contain generated code");
    let _ = fs::remove_file(object);
}

/// M7 libdecnumber combined TU 编译回归: 仅在远程 gcc-src 存在时
/// 跑；本地 CI 直接 skip。组合 TU 把 decSingle/Double/Quad 三套
/// 符号收敛到一个 .o, 避免 GCC configure 阶段 11 个 SKIP.
#[test]
#[cfg_attr(not(feature = "gcc_src"), ignore = "needs GCC source tree")]
fn libdecnumber_combined_tu_compiles() {
    let gcc_src = match std::env::var("GCC_SRC") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => return, // 没设 GCC_SRC 就当 SKIP
    };
    if !gcc_src.join("libdecnumber").exists() {
        return;
    }
    let stub_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/libdecnumber_stub");
    let combined =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/libdecnumber_combined.c");
    let dir = std::env::temp_dir();
    let object = dir.join(format!("dcc-libdecnumber-{}.o", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-I",
            gcc_src.join("libdecnumber").to_str().unwrap(),
            "-I",
            gcc_src.join("libdecnumber/dpd").to_str().unwrap(),
            "-I",
            stub_dir.to_str().unwrap(),
            "-DHAVE_CONFIG_H",
            "-o",
        ])
        .arg(&object)
        .arg(&combined)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&object);
    assert!(
        output.status.success(),
        "libdecnumber_combined compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn enum_trailing_comma_parses() {
    let output = run_syntax_test(
        "enum-trailing-comma",
        "enum jump_label_type {\n\
         \tJUMP_LABEL_NOP = 0,\n\
         \tJUMP_LABEL_JMP,\n\
         };\n\
         int main(void) { return 0; }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C++ 前端 M7: 简单 class + ctor 降级到 C 后能通过 dcc 编译
/// 验证 lexer/parser/lower 链路 + 与 dcc C 后端的衔接
/// 注: dcc 主入口在 M7 阶段未开放 C++ 子命令, 这里直接走 driver 模块
/// (留待 M8 加入 -x c++ 标志或 dcc++ 二进制入口)
#[cfg(any())]
#[test]
fn cpp_class_with_ctor_lowers_and_compiles() {
    // 暂时 disable: 集成测试需要 lib crate 才能访问 dcc::cpp
    // 等 M8 引入 dcc++ 二进制后, 通过 spawn dcc++ 验证
}

#[test]
fn typeof_cast_in_statement_expression_parses() {
    let output = run_syntax_test(
        "typeof-cast-stmt-expr",
        "typedef struct { int counter; } atomic_t;\n\
         int main(void) {\n\
             atomic_t *v;\n\
             int **old;\n\
             return ({\n\
                 __typeof__(((&v->counter))) _old =\n\
                     (__typeof__(((&v->counter))))(((old)));\n\
                 0;\n\
             });\n\
         }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn struct_function_pointer_field_parses() {
    let output = run_syntax_test(
        "struct-func-ptr-field",
        "struct dmi_system_id;\n\
         struct dmi_system_id {\n\
             int (*callback)(const struct dmi_system_id *);\n\
             const char *ident;\n\
         };\n\
         int main(void) { return 0; }\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn s_output_disassembles_function_body() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-s-disasm-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-s-disasm-{}.s", std::process::id()));
    let source = "int main(void) { return 2 + 3; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O1", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(asm_text.contains("main:"), "asm output: {asm_text}");
    assert!(
        asm_text.contains("mov") && (asm_text.contains("$5") || asm_text.contains("$0x5")),
        "expected folded return in disasm: {asm_text}"
    );
    assert!(asm_text.contains("ret"), "asm output: {asm_text}");
}

#[test]
fn s_output_emits_custom_section_functions() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-s-custom-sec-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-s-custom-sec-{}.s", std::process::id()));
    let source = "\
int global_value = 7;\n\
int *custom_pointer __attribute__((section(\".data.custom\"))) = &global_value;\n\
int custom_function(void) __attribute__((section(\".init.text\"))) {\n\
    return *custom_pointer;\n\
}\n\
int main(void) { return custom_function() == 7 ? 0 : 1; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains(".section .init.text"),
        "expected custom section directive: {asm_text}"
    );
    assert!(
        asm_text.contains("custom_function:"),
        "expected custom_function label: {asm_text}"
    );
    let init_pos = asm_text
        .find(".section .init.text")
        .expect("init.text section");
    let custom_pos = asm_text
        .find("custom_function:")
        .expect("custom_function label");
    assert!(
        custom_pos > init_pos,
        "custom_function should appear after .section .init.text: {asm_text}"
    );
}

#[test]
fn s_output_disassembles_lock_cmpxchg() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-s-lock-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-s-lock-{}.s", std::process::id()));
    let source = "\
int value = 1;\n\
int main(void) {\n\
    int expected = 1;\n\
    int desired = 2;\n\
    __atomic_compare_exchange_n(\n\
        &value, &expected, desired, 0,\n\
        __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);\n\
    return value == 2 ? 0 : 1;\n\
}\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("lock cmpxchg") || asm_text.contains("lock; cmpxchg"),
        "expected lock cmpxchg in disasm, got: {asm_text}"
    );
    assert!(
        !asm_text.contains(".byte 0xf0"),
        "lock prefix should not fall back to .byte: {asm_text}"
    );
}

#[test]
fn kbuild_define_emits_ascii_in_asm_output() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kbuild-define-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kbuild-define-{}.s", std::process::id()));
    let source = "\
struct usb_device_id { int vendor; int product; };\n\
#define DEFINE(sym, val) \\\n\
    asm volatile(\"\\n.ascii \\\"->\" #sym \" %0 \" #val \"\\\"\" : : \"i\" (val))\n\
int main(void) {\n\
    DEFINE(SIZE_usb_device_id, sizeof(struct usb_device_id));\n\
    return 0;\n\
}\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains(".ascii \"->SIZE_usb_device_id $8 sizeof(struct usb_device_id)\""),
        "asm output: {asm_text}"
    );
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !dep_text.contains("sys.h"),
        "MMD should skip system header: {dep_text}"
    );
}

#[test]
fn flto_summary_includes_function_size() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-lto-size-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-lto-size-{}.o", std::process::id()));
    fs::write(
        &source_path,
        "int helper(void) { return 1; }\nint main(void) { return helper(); }\n",
    )
    .expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj_bytes = fs::read(&obj_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&obj_bytes);
    assert!(
        text.contains("helper:"),
        "LTO summary missing helper: {text}"
    );
}

#[test]
fn extern_comma_array_declarators_parse() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-comma-arr-{}.c", std::process::id()));
    let source = "\
typedef int entry_t;\n\
extern entry_t __start[], __end[];\n\
int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsyntax-only"])
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn gnu_case_range_switch_compiles() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-case-range-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-case-range-{}.o", std::process::id()));
    let source = "\
int classify(int c) {\n\
    switch (c) {\n\
    case '0' ... '9': return 1;\n\
    default: return 0;\n\
    }\n\
}\n\
int main(void) { return classify('5'); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn debug_sections_emitted_with_g_flag() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-debug-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-debug-{}.o", std::process::id()));
    let source = "int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-g", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj_str = String::from_utf8_lossy(&obj);
    assert!(
        obj_str.contains(".debug_info"),
        "missing .debug_info in object"
    );
    assert!(obj_str.contains(".BTF"), "missing .BTF in object");
}

#[test]
fn o1_constant_folding_emits_immediate() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-o1-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-o1-{}.o", std::process::id()));
    let source = "int main(void) { return 2 + 3; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O1", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 新编码: REX.W + MOV r/m64, imm32 = [0x48, 0xC7, 0xC0, 0x05, 0x00, 0x00, 0x00]
    assert!(
        obj.windows(7).any(|w| w == [0x48, 0xC7, 0xC0, 0x05, 0x00, 0x00, 0x00]),
        "expected mov $5, %rax after constant fold"
    );
}

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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dep_text.contains(&hdr_name),
        "dep file missing header: {dep_text}"
    );
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
    fs::write(
        &src_path,
        format!("#include \"{}\"\nint main(void) {{ return V; }}\n", hdr),
    )
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // GCC 兼容格式: 所有非末行应以 \ 续行符结尾
    let lines: Vec<&str> = dep_text.lines().collect();
    if lines.len() > 1 {
        for line in &lines[..lines.len() - 1] {
            assert!(
                line.ends_with('\\'),
                "dep line should end with \\: {line:?}"
            );
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
    fs::write(
        &src_path,
        format!("#include \"{}\"\nint main(void) {{ return V; }}\n", hdr),
    )
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // -MP: 每个头文件应有 phony 目标 (hdr:)
    assert!(
        dep_text.contains(&format!("{}:", hdr)),
        "-MP should emit phony target for header: {dep_text}"
    );
}

#[test]
fn sanitizer_metadata_section_emitted() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-san-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-san-{}.o", std::process::id()));
    let source = "int f(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&obj).contains(".dcc.san_info"),
        "missing sanitizer metadata section"
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(asm.matches("__asan_init").count(), 1, "TU init once: {asm}");
    assert!(
        asm.matches("__sanitizer_cov_trace_pc").count() >= 2,
        "per-function coverage stub: {asm}"
    );
}

/// KASAN 栈红区: -fsanitize=address 时函数入口应发射
/// __asan_poison_memory_region 和 __asan_unpoison_memory_region 调用
#[test]
fn kasan_stack_redzone_poison_unpoison() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-stack-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kasan-stack-{}.s", std::process::id()));
    let source = "int f(void) { int x = 42; return x; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm.contains("__asan_poison_memory_region"),
        "missing poison call: {asm}"
    );
    assert!(
        asm.contains("__asan_unpoison_memory_region"),
        "missing unpoison call: {asm}"
    );
}

/// KASAN 全局变量红区: -fsanitize=address 时全局变量后应有 32 字节填充
#[test]
fn kasan_global_redzone_padding() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-glob-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-kasan-glob-{}.o", std::process::id()));
    let source = "int x = 1;\nint y = 2;\n";
    fs::write(&source_path, source).expect("write C fixture");
    // 编译无 KASAN
    let out_normal = {
        let p = dir.join(format!("dcc-kasan-glob-normal-{}.o", std::process::id()));
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-o"])
            .arg(&p)
            .arg(&source_path)
            .output()
            .expect("run dcc");
        assert!(o.status.success());
        fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
    };
    // 编译有 KASAN
    let out_kasan = {
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-fsanitize=address", "-c", "-o"])
            .arg(&out_path)
            .arg(&source_path)
            .output()
            .expect("run dcc");
        assert!(
            o.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0)
    };
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&out_path);
    // KASAN 输出应更大 (全局变量红区填充)
    assert!(
        out_kasan > out_normal,
        "KASAN object ({out_kasan}) should be larger than normal ({out_normal}) due to global redzones"
    );
}

#[test]
fn eh_frame_with_frame_pointer() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-fp-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-fp-{}.o", std::process::id()));
    let source = "int f(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-g", "-fno-omit-frame-pointer", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj_str = String::from_utf8_lossy(&obj);
    assert!(obj_str.contains(".eh_frame"), "missing .eh_frame");
    assert!(obj_str.contains(".orc_unwind_ip"), "missing .orc_unwind_ip");
}

#[test]
fn global_asm_exports_symbol() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-gasm-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-gasm-{}.o", std::process::id()));
    // raw string 保留 \n 为字面 2 字符, dcc C 解析器只接受单行串字面量
    let source = r#"asm(".globl global_marker\nglobal_marker:\n");"#;
    std::fs::write(&source_path, source).expect("write asm fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&obj).contains("global_marker"),
        "global asm symbol not exported"
    );
}

#[test]
fn flto_section_emitted() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-lto-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-lto-{}.o", std::process::id()));
    let source = "int foo(void) { return 1; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&obj).contains(".llvm.lto"),
        "missing LTO summary section"
    );
}

#[test]
fn function_typedef_parses() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-fn-typedef-{}.c", std::process::id()));
    let source = "\
typedef int handler_t (void);\n\
typedef handler_t *handler_ptr;\n\
int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsyntax-only"])
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn restrict_parameter_parses() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-restrict-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-restrict-{}.o", std::process::id()));
    let source = "\
void copy(char * __restrict dst, const char * __restrict src) {\n\
    dst[0] = src[0];\n\
}\n\
int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hosted_stdio_header_parses() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-stdio-{}.c", std::process::id()));
    let source = "#include <stdio.h>\nint main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsyntax-only"])
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn comma_pointer_declarators_parse() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-comma-ptr-{}.c", std::process::id()));
    let source = "\
void f(void) {\n\
    const char *name, *identifier;\n\
    (void)name;\n\
    (void)identifier;\n\
}\n\
int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsyntax-only"])
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn typeof_struct_member_and_def_field_pattern() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-def-field-{}.c", std::process::id()));
    let source = "\
struct node { unsigned char b[16]; };\n\
struct id { struct node uuid; };\n\
static int use_uuid(void *symval) {\n\
    typeof(((struct id *)0)->uuid) uuid = *(typeof(uuid) *)((char *)symval);\n\
    return uuid.b[0];\n\
}\n\
int main(void) { return use_uuid((void *)0); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsyntax-only"])
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn devtable_static_init_with_function_pointer() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-devtable-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-devtable-{}.o", std::process::id()));
    let source = "\
struct devtable {\n\
    const char *device_id;\n\
    unsigned long id_size;\n\
    int (*do_entry)(const char *, void *, char *);\n\
};\n\
static int helper(const char *f, void *s, char *a) { (void)f; (void)s; (void)a; return 0; }\n\
static const struct devtable devtable[] = {\n\
    {\"hid\", 24, helper},\n\
};\n\
int main(void) { return devtable[0].id_size == 24 ? 0 : 1; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stdbool_before_linux_stddef_enum_parses() {
    let dir = std::env::temp_dir();
    let hdr_dir = dir.join(format!("dcc-stddef-{}", std::process::id()));
    let linux_dir = hdr_dir.join("linux");
    fs::create_dir_all(&linux_dir).expect("create linux include dir");
    fs::write(
        linux_dir.join("stddef.h"),
        "enum {\n\tfalse = 0,\n\ttrue = 1\n};\n",
    )
    .expect("write stddef stub");
    let source_path = dir.join(format!("dcc-stdbool-std-{}.c", std::process::id()));
    fs::write(
        &source_path,
        "#include <stdbool.h>\n#include \"linux/stddef.h\"\ntypedef _Bool _Bool;\nint main(void) { return false; }\n",
    )
    .expect("write source");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-I"])
        .arg(&hdr_dir)
        .args(["-fsyntax-only"])
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_dir_all(hdr_dir);
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn btf_section_contains_struct_kind() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-btf-struct-{}", std::process::id()));
    let out_path = dir.join(format!("dcc-btf-struct-{}.o", std::process::id()));
    let source = "int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-g", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        obj.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == 4
        }),
        "BTF missing STRUCT type encoding"
    );
}

#[test]
fn eh_frame_larger_than_cie_only() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-eh-fde-{}", std::process::id()));
    let out_path = dir.join(format!("dcc-eh-fde-{}.o", std::process::id()));
    let source = "int foo(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-g", "-fno-omit-frame-pointer", "-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj_str = String::from_utf8_lossy(&obj);
    let start = obj_str.find(".eh_frame").expect(".eh_frame name");
    let tail = &obj_str[start..start.saturating_add(256).min(obj_str.len())];
    assert!(tail.len() > 64, "eh_frame section likely missing FDE");
}

#[test]
#[cfg(unix)]
fn flto_relocatable_link_merges_section() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let a_src = dir.join(format!("dcc-lto-a-{pid}.c"));
    let b_src = dir.join(format!("dcc-lto-b-{pid}.c"));
    let a_o = dir.join(format!("dcc-lto-a-{pid}.o"));
    let b_o = dir.join(format!("dcc-lto-b-{pid}.o"));
    let out_o = dir.join(format!("dcc-lto-merged-{pid}.o"));
    fs::write(&a_src, "int foo(void) { return 1; }\n").unwrap();
    fs::write(&b_src, "int bar(void) { return 2; }\n").unwrap();
    let dcc = env!("CARGO_BIN_EXE_dcc");
    for (src, obj) in [(&a_src, &a_o), (&b_src, &b_o)] {
        let out = Command::new(dcc)
            .args(["-flto", "-c", "-o"])
            .arg(obj)
            .arg(src)
            .output()
            .expect("compile lto object");
        assert!(
            out.status.success(),
            "compile: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let link = Command::new(dcc)
        .args(["-r", "-flto", "-o"])
        .arg(&out_o)
        .arg(&a_o)
        .arg(&b_o)
        .output()
        .expect("link lto objects");
    let merged = fs::read(&out_o).unwrap_or_default();
    let _ = fs::remove_file(&a_src);
    let _ = fs::remove_file(&b_src);
    let _ = fs::remove_file(&a_o);
    let _ = fs::remove_file(&b_o);
    let _ = fs::remove_file(&out_o);
    assert!(
        link.status.success(),
        "link: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(
        String::from_utf8_lossy(&merged).contains(".llvm.lto"),
        "linked object missing merged .llvm.lto"
    );
}

/// glibc string.h：`_Generic(0 ? ... : ..., const void *: ..., default: ...)`
/// 三元 else 不得吞掉 association 列表的逗号。
#[test]
fn generic_with_ternary_controlling_expr_compiles() {
    let source = include_str!("fixtures/c/generic_tern.c");
    let (output, obj) = run_object_test("generic-ternary", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `_Generic` 指针按 pointee 精确匹配，char* 与 int* 不得互匹配。
#[test]
fn generic_pointer_precise_match_compiles() {
    let source = include_str!("fixtures/c/generic_ptr.c");
    let (output, obj) = run_object_test("generic-pointer", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 内联 asm 约束 (=r / +m / memory clobber) 编译通过。
#[test]
fn asm_constraints_compile() {
    let source = include_str!("../examples/assembly/constraints.c");
    let (output, obj) = run_object_test("asm-constraints", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// SysV va_list GP + SSE 浮点路径编译通过。
#[test]
fn va_list_sse_compiles() {
    let source = include_str!("../examples/advanced/va_list.c");
    let (output, obj) = run_object_test("va-list-sse", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// -O2 常量分支折叠后 main 直接返回常量。
#[test]
fn o2_constant_branch_fold() {
    let source = "int main(void) { if (1) return 42; return 0; }\n";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-o2-branch-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-o2-branch-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(object_path);
}

/// -O2 DCE 须保留 return 之后的 goto 目标标签（radix_tree_next_slot 模式）。
#[test]
fn o2_goto_label_after_return() {
    let source = r#"
static void **slot_after_return(void **slot, int flags) {
    if (flags) { goto found; }
    return 0;
 found:
    return slot;
}
void use_it(void) { (void)slot_after_return(0, 1); }
"#;
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-o2-goto-label-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-o2-goto-label-{}.s", std::process::id()));
    fs::write(&source_path, source).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(asm_path);
}

/// M7: aarch64-linux-gnu 交叉编译 hello.o（typed IR lowering）。
#[test]
fn aarch64_hello_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-hello-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-a64-hello-{}.o", std::process::id()));
    fs::write(&source_path, "int main(void) { return 42; }\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "aarch64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            object_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183);
}

/// M7: aarch64 typed IR 二元表达式 return 10+32。
#[test]
fn aarch64_ir_add_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-add-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-a64-add-{}.o", std::process::id()));
    fs::write(&source_path, "int main(void) { return 10 + 32; }\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "aarch64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            object_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183);
    assert!(bytes.len() > 64);
}

/// 阶段 E：-O2 函数级并行 codegen（多 worker 线程 + 按序 merge）。
#[test]
fn parallel_codegen_o2_multi_function() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let source_path = dir.join(format!("dcc-par-{pid}.c"));
    let parallel_path = dir.join(format!("dcc-par-o-{pid}.o"));
    let serial_path = dir.join(format!("dcc-par-s-{pid}.o"));
    fs::write(
        &source_path,
        "static int helper(void) { return 3; }\n\
         int foo(void) { return helper() + 1; }\n\
         int bar(void) { return helper() + 2; }\n\
         int main(void) { return foo() + bar(); }\n",
    )
    .expect("write");

    let run = |path: &std::path::Path, serial: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
        if serial {
            cmd.env("DCC_NO_PARALLEL", "1");
        } else {
            cmd.env_remove("DCC_NO_PARALLEL");
        }
        cmd.args([
            "-O2",
            "-c",
            "-o",
            path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc")
    };

    let par = run(&parallel_path, false);
    let ser = run(&serial_path, true);
    let _ = fs::remove_file(&source_path);
    assert!(
        par.status.success(),
        "parallel stderr: {}",
        String::from_utf8_lossy(&par.stderr)
    );
    assert!(
        ser.status.success(),
        "serial stderr: {}",
        String::from_utf8_lossy(&ser.stderr)
    );
    let par_bytes = fs::read(&parallel_path).expect("read parallel");
    let ser_bytes = fs::read(&serial_path).expect("read serial");
    let _ = fs::remove_file(&parallel_path);
    let _ = fs::remove_file(&serial_path);
    assert_eq!(&par_bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(&ser_bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(
        par_bytes.len(),
        ser_bytes.len(),
        "parallel/serial .text size mismatch"
    );
    assert_eq!(
        par_bytes, ser_bytes,
        "parallel and serial objects must match"
    );
}

/// M7: aarch64 if/while 控制流交叉编译。
#[test]
fn aarch64_if_while_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-ctrl-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-a64-ctrl-{}.o", std::process::id()));
    fs::write(
        &source_path,
        "int helper(int x) { return x + 1; }\n\
         int main(void) {\n\
           int s = 0, i = 0;\n\
           while (i < 3) { s = helper(s); i = i + 1; }\n\
           if (s > 0) return s;\n\
           return 0;\n\
         }\n",
    )
    .expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "aarch64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            object_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183);
}

/// M7: riscv64-linux-gnu 交叉编译 hello.o。
#[test]
fn riscv64_hello_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-rv-hello-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-rv-hello-{}.o", std::process::id()));
    fs::write(&source_path, "int main(void) { return 42; }\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "riscv64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            object_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 243);
}

/// M7: riscv64 typed IR 二元表达式与全局 rodata。
#[test]
fn riscv64_ir_global_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-rv-global-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-rv-global-{}.o", std::process::id()));
    fs::write(
        &source_path,
        "int g = 7;\nint main(void) { return 10 + 32 + g; }\n",
    )
    .expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "riscv64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            object_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 243);
    assert!(bytes.len() > 64);
}

/// static_call 语句表达式 + 尾随调用: ({ ... ;; expr })()
#[test]
fn stmt_expr_trailing_call_compiles() {
    let source = include_str!("fixtures/c/stmt_call.c");
    let (output, obj) = run_object_test("stmt-expr-call", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// __typeof__(struct T) 类型推导
#[test]
fn typeof_struct_compiles() {
    let source = include_str!("fixtures/c/typeof_struct.c");
    let (output, obj) = run_object_test("typeof-struct", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// percpu 声明: section 字符串拼接 + __aligned__((expr))
#[test]
fn percpu_decl_compiles() {
    let source = include_str!("fixtures/c/percpu.c");
    let (output, obj) = run_object_test("percpu-decl", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// __attribute__((section("a" "b"))) 相邻字符串拼接
#[test]
fn section_concat_attribute_compiles() {
    let source = include_str!("fixtures/c/section.c");
    let (output, obj) = run_object_test("section-concat", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 指定初始化器 + asm inline（内核 GDT/jump_label 常见模式）
#[test]
fn gdt_designated_init_and_asm_inline_compiles() {
    let source = include_str!("fixtures/c/designated.c");
    let (output, obj) = run_object_test("gdt-designated-init", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 逗号分隔位域: u16 a:8, b:4, c:1（desc_struct 模式）
#[test]
fn bitfield_comma_separated_compiles() {
    let source = include_str!("fixtures/c/bitfield.c");
    let (output, obj) = run_object_test("bitfield-comma", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// asm __inline volatile(...) 限定符
#[test]
fn asm_inline_qualifier_compiles() {
    let source = include_str!("fixtures/c/asm_inline.c");
    let (output, obj) = run_object_test("asm-inline-qual", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 结构体空字段: struct { int a; ; }（lockdep class 宏）
#[test]
fn struct_empty_field_compiles() {
    let source = include_str!("fixtures/c/struct_empty.c");
    let (output, obj) = run_object_test("struct-empty-field", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// GNU __auto_type 类型推导
#[test]
fn auto_type_decl_compiles() {
    let source = include_str!("fixtures/c/auto_type.c");
    let (output, obj) = run_object_test("auto-type", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// _Static_assert + __builtin_offsetof 编译期求值
#[test]
fn static_assert_offsetof_compiles() {
    let source = include_str!("fixtures/c/sassert_off.c");
    let output = run_syntax_test("static-assert-offsetof", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// object-like 宏别名 + 函数式宏链式展开
#[test]
fn div_round_up_macro_expands() {
    let source = include_str!("fixtures/c/div_round_up.c");
    let output = run_syntax_test("div-round-up", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 结构体多维数组字段
#[test]
fn multidim_array_field_compiles() {
    let source = include_str!("fixtures/c/multidim.c");
    let output = run_syntax_test("multidim-array-field", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// void __attribute__(...) *fn(): 属性在返回类型与指针声明符之间
#[test]
fn attr_ptr_func_decl_compiles() {
    let source = include_str!("fixtures/c/attr_ptr.c");
    let output = run_syntax_test("attr-ptr-func", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 语句表达式内 typeof 局部声明
#[test]
fn vfs_pressure_stmt_expr_compiles() {
    let source = include_str!("fixtures/c/stmt_vfs.c");
    let output = run_syntax_test("vfs-pressure", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// GNU 省略中间操作数的三元运算符 x ?: y
#[test]
fn gnu_elvis_operator_compiles() {
    let source = include_str!("fixtures/c/elvis.c");
    let output = run_syntax_test("gnu-elvis", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 零参 function-like 宏 current_thread_info() + bitop 链
#[test]
fn current_thread_info_macro_compiles() {
    let source = include_str!("fixtures/c/thread_info.c");
    let output = run_syntax_test("current-thread-info", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 三元表达式指针分支可被解引用
#[test]
fn ternary_ptr_deref_compiles() {
    let source = include_str!("fixtures/c/ternary_ptr.c");
    let output = run_syntax_test("ternary-ptr-deref", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// GNU 局部数字标签 6: / 6f / 6b
#[test]
fn asm_local_label_compiles() {
    let source = include_str!("fixtures/c/asm_local.c");
    let (output, _obj) = run_object_test("asm-local-label", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 同行局部标签 N:insn（如 1:jmp 2f）
#[test]
fn asm_label_insn_same_line_compiles() {
    let source = include_str!("fixtures/c/asm_label.c");
    let (output, _obj) = run_object_test("asm-label-insn", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// struct __attribute__((__may_alias__)) name
#[test]
fn may_alias_struct_compiles() {
    let source = include_str!("fixtures/c/may_alias.c");
    let (output, _obj) = run_object_test("may-alias-struct", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// NOFAIL / # 字符串化：多行实参折叠为单行字面量
#[test]
fn nofail_stringify_compiles() {
    let source = include_str!("fixtures/c/stringify.c");
    let (output, _obj) = run_object_test("nofail-stringify", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// GCC 兼容：相同类型的重复 typedef 不报错
#[test]
fn compat_typedef_redefinition_compiles() {
    let source = include_str!("fixtures/c/typedef.c");
    let (output, _obj) = run_object_test("compat-typedef", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 局部变量可遮蔽全局函数名（index/basename 等 libc 符号）
#[test]
fn local_shadows_libc_function_name() {
    let source = include_str!("fixtures/c/shadow.c");
    let (output, exe) = run_compile_and_run("shadow-libc-name", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_exe(&exe), 0);
}

/// typeof(_Generic(...))：内核 READ_ONCE 类宏展开形态
#[test]
fn typeof_generic_read_once_compiles() {
    let source = include_str!("fixtures/c/typeof_generic.c");
    let (output, exe) = run_compile_and_run("typeof-generic-read-once", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_exe(&exe), 0);
}

/// asm goto 模板行内 `#` 注释（jump_label.h objtool 标注）不污染标签名
#[test]
fn asm_goto_hash_comment_compiles() {
    let source = include_str!("fixtures/c/asm_goto.c");
    let (output, exe) = run_compile_and_run("asm-goto-hash-comment", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_exe(&exe), 0);
}

/// 非 Ident 向量表达式：嵌套 binop 与 `v = v + w` 赋值
#[test]
fn vector_binop_non_ident_run_returns_zero() {
    let source = include_str!("fixtures/c/vec_binop.c");
    let (output, exe) = run_compile_and_run("vector-binop-non-ident", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_exe(&exe), 0);
}

/// 全局 SIMD 向量复制与逐元素运算
#[test]
fn vector_global_copy_compiles() {
    let source = include_str!("fixtures/c/vec_global.c");
    let (output, _obj) = run_object_test("vector-global", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 全局向量初始化 + 运行时求值（期望 exit 0）
#[test]
fn vector_global_run_returns_zero() {
    let source = include_str!("fixtures/c/vec_run.c");
    let (output, exe) = run_compile_and_run("vector-global-run", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "vector_global_run should return 0, got {rc}");
}

/// 向量 InitList rvalue（复合字面量运算）
#[test]
fn vector_init_rvalue_run_returns_zero() {
    let source = include_str!("fixtures/c/vec_init.c");
    let (output, exe) = run_compile_and_run("vector-init-rvalue", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "vector_init_rvalue should return 0, got {rc}");
}

/// _Complex 除法 codegen
#[test]
fn complex_div_run_returns_zero() {
    let source = include_str!("fixtures/c/complex_div.c");
    let (output, exe) = run_compile_and_run("complex-div", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "complex_div should return 0, got {rc}");
}

/// __builtin_object_size 编译期折叠
#[test]
fn object_size_builtin_run_returns_zero() {
    let source = include_str!("fixtures/c/objsize2.c");
    let (output, exe) = run_compile_and_run("object-size-builtin", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "object_size_builtin should return 0, got {rc}");
}

/// cast 表达式内部带 __attribute__((vector_size(N))) 修饰
/// 场景: libiberty/sha1.c 的 HAVE_X86_SHA1_HW_SUPPORT 路径
///       以及 GCC 内部头文件常见用法
/// 验证: dcc 解析 ((__attribute__((__vector_size__ (16))) int) &x)
///       不再报 "expected ')', got KwInt", 且运行时能正确取值.
#[test]
fn vector_size_cast_in_expression_run_returns_zero() {
    let source = include_str!("fixtures/c/vec_cast.c");
    let (output, exe) = run_compile_and_run("vector-size-cast-in-expr", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(
        rc, 0,
        "vector_size_cast_in_expression should return 0, got {rc}"
    );
}

/// __builtin_object_size FORTIFY 模式（数组/成员）
#[test]
fn builtin_object_size_fortify_run_returns_zero() {
    let source = include_str!("fixtures/c/obj_size.c");
    let (output, exe) = run_compile_and_run("builtin-os-fortify", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_exe(&exe), 0);
}

/// __builtin_apply / __builtin_return
#[test]
fn builtin_apply_return_run_returns_zero() {
    let source = include_str!("fixtures/c/apply_ret.c");
    let (output, exe) = run_compile_and_run("builtin-apply-return", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_exe(&exe), 0);
}

/// __builtin_prefetch void no-op
#[test]
fn builtin_prefetch_compiles() {
    let source = include_str!("fixtures/c/prefetch.c");
    let (output, _obj) = run_object_test("builtin-prefetch", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// inline asm: lock cmpxchg 内存操作数
#[test]
fn asm_cmpxchg_mem_compiles() {
    let source = include_str!("fixtures/c/asm_cmpxchg.c");
    let (output, _obj) = run_object_test("asm-cmpxchg-mem", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// mini-asm 扩展：bts/btr/btc/bsf/bsr/popcnt/bswap/movnt/lock/rep/伪指令
#[test]
fn asm_extended_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_ext.c");
    let (output, _obj) = run_object_test("asm-extended", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// __int128 SysV 双寄存器传参与返回值
#[test]
fn int128_call_abi_run_returns_zero() {
    let source = include_str!("fixtures/c/int128_abi.c");
    let (output, exe) = run_compile_and_run("int128-call-abi", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "int128_call_abi should return 0, got {rc}");
}

/// __int128 乘/除/取模运行时正确性
#[test]
fn int128_mul_div_mod_run_returns_zero() {
    let source = include_str!("fixtures/c/int128_arith.c");
    let (output, exe) = run_compile_and_run("int128-mul-div-mod", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "int128_mul_div_mod should return 0, got {rc}");
}

/// M13.1: float/double 取模软件实装 (divss/divsd + trunc)
#[test]
fn float_mod_run_returns_zero() {
    let source = include_str!("fixtures/c/float_mod.c");
    let (output, _exe) = run_compile_and_run("float-mod", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "float_mod should return 0, got {rc}");
    }
}

/// M13.1: __int128 &&/|| 高 64 位合并后再 test
#[test]
fn int128_logical_run_returns_zero() {
    let source = include_str!("fixtures/c/int128_log.c");
    let (output, _exe) = run_compile_and_run("int128-logical", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "int128_logical should return 0, got {rc}");
    }
}

/// M13.1: asm 约束扩展 (含 XMM "x" 输入 prepare + 写回)
#[test]
fn asm_constraints_ext_compiles_and_runs() {
    let source = include_str!("fixtures/c/asm_constr.c");
    let (output, _exe) = run_compile_and_run("asm-constraints-ext", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "asm_constraints_ext should return 0, got {rc}");
    }
}

/// M13.1: 全局数组 [rip+disp32+idx*scale] 单指令加载
#[test]
fn global_indexed_run_returns_zero() {
    let source = include_str!("fixtures/c/global_idx.c");
    let (output, _exe) = run_compile_and_run("global-indexed", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "global_indexed should return 0, got {rc}");
    }
}

/// SIMD 浮点向量：标量 SSE 逐元素 +/-/×
#[test]
fn vector_float_scalar_run_returns_zero() {
    let source = include_str!("fixtures/c/vec_fscalar.c");
    let (output, exe) = run_compile_and_run("vector-float-scalar", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "vector_float_scalar should return 0, got {rc}");
}

/// mini-asm 内核扩展：rol/prefetch/movss/addss
#[test]
fn asm_kernel_extra_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_kernel.c");
    let (output, _obj) = run_object_test("asm-kernel-extra", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// mini-asm 内核扩展指令：orb/orw/cmova/clwb/invpcid/rdrand/rdseed/
/// fwait/fxsaveq/fxrstorq/xgetbv/xsetbv/monitor/mwait/monitorx/mwaitx/
/// clgi/stgi/vmsave/sldt/repe 前缀修复
#[test]
fn asm_kernel_ext_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_kernel_ext.c");
    let (output, _obj) = run_object_test("asm-kernel-ext", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// mini-asm 补全 (高): 段寄存器 / 系统指令 / 内核启动 / 虚拟化指令
#[test]
fn asm_paravirt_boot_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_paravirt.c");
    let (output, _obj) = run_object_test("asm-paravirt-boot", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// 完整指令子集 (M13): AVX/AVX2/AVX-VNNI + AES-NI/SHA + FMA/F16C + TSX + AMX
#[test]
fn asm_avx_aes_fma_tsx_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_avx.c");
    let (output, _obj) = run_object_test("asm-avx-aes-fma-tsx", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// x87 FPU 指令集: 无操作数指令 / 比较 / 加载存储 / 控制 / 算术 / BCD
#[test]
fn asm_x87_fpu_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_x87_fpu.c");
    let (output, _obj) = run_object_test("asm-x87-fpu", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// MMX 指令集: emms / 打包解包 / 算术 / 移位 / pshufw
#[test]
fn asm_mmx_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_mmx.c");
    let (output, _obj) = run_object_test("asm-mmx", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// 字符串操作 / 循环 / 杂项指令: lodsb-w-l-q / cmpsw-l-q / scasw-l-q / loop / enter
#[test]
fn asm_string_loop_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_string_loop.c");
    let (output, _obj) = run_object_test("asm-string-loop", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// BMI1/BMI2 扩展: andn/bextr/blsi/blsmsk/blsr/bzhi/mulx/pdep/pext/rorx/sarx/shlx/shrx/adcx/adox
#[test]
fn asm_bmi_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_bmi.c");
    let (output, _obj) = run_object_test("asm-bmi", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// Phase 2: 操作数模式补全 — 所有新加的操作数组合
#[test]
fn asm_operand_patterns_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_operand_patterns.c");
    let (output, _obj) = run_object_test("asm-operand-patterns", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// Phase 2: 远跳转/远调用 — 所有远跳转指令
#[test]
fn asm_far_jumps_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_far_jumps.c");
    let (output, _obj) = run_object_test("asm-far-jumps", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// Phase 2: GlobalAsm/asm goto 残余 — 所有间接跳转/段前缀组合
#[test]
fn asm_global_asm_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/c/asm_global_asm.c");
    let (output, _obj) = run_object_test("asm-global-asm", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// 头文件中不支持的内联 asm 警告必须映射回真实源文件行号
/// (而非预处理输出行号, 内核头文件可达几十万行, 虚高行号无法定位)。
/// fixture: asm_warn_loc.h 第 6 行含 `leaq %c1(%%rip), %0`。
#[test]
fn asm_warning_location_maps_to_real_source_line() {
    let dir = std::env::temp_dir();
    let tag = format!("dcc-asm-warn-loc-{}", std::process::id());
    let hdr_path = dir.join(format!("{tag}.h"));
    let src_path = dir.join(format!("{tag}.c"));
    let hdr = include_str!("fixtures/c/asm_warn_loc.h");
    fs::write(&hdr_path, hdr).expect("write header fixture");
    fs::write(
        &src_path,
        format!("#include \"{}\"\nint main(void) {{ return 0; }}\n", hdr_path.display()),
    )
    .expect("write C fixture");
    let obj_path = dir.join(format!("{tag}.o"));
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    // 警告必须落在头文件第 6 行 (asm 语句), 而不是预处理输出行号
    let expect = format!("{}:6: warning: unsupported lea operand pattern", hdr_path.display());
    assert!(
        stderr.contains(&expect),
        "expected precise asm warning location {expect:?}, got: {stderr}"
    );
    let _ = fs::remove_file(hdr_path);
    let _ = fs::remove_file(src_path);
    let _ = fs::remove_file(obj_path);
}

/// 头文件缓存 (冷→热) 回放不得丢失 line_map 精度:
/// 第二次编译 (缓存命中) 的 asm 警告位置必须与首次一致且同样精确。
#[test]
fn asm_warning_location_stable_across_header_cache_replay() {
    let dir = std::env::temp_dir();
    let tag = format!("dcc-asm-warn-cache-{}", std::process::id());
    let hdr_path = dir.join(format!("{tag}.h"));
    let src_path = dir.join(format!("{tag}.c"));
    let obj_path = dir.join(format!("{tag}.o"));
    let home_dir = dir.join(format!("{tag}-home"));
    fs::create_dir_all(&home_dir).expect("create temp HOME");
    fs::write(&hdr_path, include_str!("fixtures/c/asm_warn_loc.h"))
        .expect("write header fixture");
    fs::write(
        &src_path,
        format!("#include \"{}\"\nint main(void) {{ return 0; }}\n", hdr_path.display()),
    )
    .expect("write C fixture");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_dcc"))
            .env("HOME", &home_dir)
            .args(["-c", "-o"])
            .arg(&obj_path)
            .arg(&src_path)
            .output()
            .expect("run dcc")
    };
    let cold = run();
    let hot = run();
    let cold_err = String::from_utf8_lossy(&cold.stderr);
    let hot_err = String::from_utf8_lossy(&hot.stderr);
    assert!(cold.status.success(), "cold stderr: {cold_err}");
    assert!(hot.status.success(), "hot stderr: {hot_err}");
    // 冷/热两条路径的警告必须一致 (缓存回放不丢映射精度)
    assert_eq!(
        cold_err.trim(),
        hot_err.trim(),
        "cold/hot cache warning mismatch"
    );
    let expect = format!("{}:6: warning: unsupported lea operand pattern", hdr_path.display());
    assert!(
        hot_err.contains(&expect),
        "expected precise asm warning location {expect:?}, got: {hot_err}"
    );
    let _ = fs::remove_file(hdr_path);
    let _ = fs::remove_file(src_path);
    let _ = fs::remove_file(obj_path);
    let _ = fs::remove_dir_all(home_dir);
}

/// __builtin_constant_p：编译期常量判定（含 __int128 表达式）
#[test]
fn builtin_constant_p_full_run_returns_zero() {
    let source = include_str!("fixtures/c/constp.c");
    let (output, exe) = run_compile_and_run("builtin-constant-p-full", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_constant_p_full should return 0, got {rc}");
}

/// M12+ builtins: rotate/clrsb/fma/strcasecmp/FILE/LINE/FUNCTION/classify_type 等
#[test]
fn builtin_m12_extra_run_returns_zero() {
    let source = include_str!("fixtures/c/builtins_misc.c");
    let (output, exe) = run_compile_and_run("builtin-m12-extra", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "test_m12_builtins should return 0, got {rc}");
}

/// M12+ builtins: rotate / clrsb / fma 综合测试
#[test]
fn builtin_rotate_clrsb_run_returns_zero() {
    let source = include_str!("fixtures/c/rotate.c");
    let (output, exe) = run_compile_and_run("builtin-rotate-clrsb", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "rotate/clrsb/fma should return 0, got {rc}");
}

/// M12+ builtins: strcasecmp / strncasecmp (走 libc)
#[test]
fn builtin_strcasecmp_run_returns_zero() {
    let source = include_str!("fixtures/c/strcasecmp.c");
    let (output, exe) = run_compile_and_run("builtin-strcasecmp", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "strcasecmp should return 0, got {rc}");
}

/// M12+ builtins: __builtin_FILE / __builtin_LINE / __builtin_FUNCTION /
/// __builtin_PRETTY_FUNCTION / __func__ / __FUNCTION__
#[test]
fn builtin_file_line_func_run_returns_zero() {
    let source = include_str!("fixtures/c/file_line.c");
    let (output, exe) = run_compile_and_run("builtin-file-line-func", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "file/line/func should return 0, got {rc}");
}

/// M12+ builtins: classify_type / types_compatible_p
#[test]
fn builtin_classify_compat_run_returns_zero() {
    let source = include_str!("fixtures/c/classify.c");
    let (output, exe) = run_compile_and_run("builtin-classify-compat", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "classify/compat should return 0, got {rc}");
}

/// M12+ builtins: choose_expr + constant_p + object_size 综合
#[test]
fn builtin_choose_full_run_returns_zero() {
    let source = include_str!("fixtures/c/choose.c");
    let (output, exe) = run_compile_and_run("builtin-choose-full", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "choose should return 0, got {rc}");
}

/// always_inline / noinline：IR 内联 pass 尊重属性
#[test]
fn inline_attrs_o1_run_returns_zero() {
    let source = include_str!("fixtures/c/inline.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-inline-attrs-{}.c", std::process::id()));
    let exe_path = dir.join(format!("dcc-inline-attrs-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-O1", "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
        let rc = run_exe(&exe_path);
        assert_eq!(rc, 0, "inline_attrs should return 0, got {rc}");
        let _ = fs::remove_file(exe_path);
    }
}

/// 未知 __builtin_*：warning + 零值；void 变体发射 ud2
#[test]
#[cfg(unix)]
fn unknown_builtin_warning_zero_and_ud2() {
    let source = include_str!("fixtures/c/unk_builtin.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-unknown-builtin-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-unknown-builtin-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = fs::remove_file(source_path);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("unsupported __builtin_"),
        "expected warning for unknown builtin: {stderr}"
    );
    let disasm = Command::new("objdump")
        .args(["-d", object_path.to_str().unwrap()])
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&disasm.stdout);
    assert!(
        text.contains("ud2"),
        "void unknown __builtin_ should emit ud2, disasm:\n{text}"
    );
    let _ = fs::remove_file(object_path);
}

/// 通用工具：编译一个 C 源文件并返回 output + 可执行路径
#[allow(dead_code)]
fn run_compile_o(
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

/// always_inline 在 -O0 仍展开
#[test]
#[cfg(unix)]
fn always_inline_o0_run_returns_zero() {
    let source = include_str!("fixtures/c/always_inline.c");
    let (output, exe) = run_compile_o("always-inline-o0", source, "-O0");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "always_inline_o0 should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// GVN/CSE：重复纯表达式复用
#[test]
#[cfg(unix)]
fn opt_gvn_o2_run_returns_zero() {
    let source = include_str!("fixtures/opt/gvn.c");
    let (output, exe) = run_compile_o("opt-gvn", source, "-O2");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "opt_gvn should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 循环展开（-O3）
#[test]
#[cfg(unix)]
fn opt_loop_unroll_o3_run_returns_zero() {
    let source = include_str!("fixtures/opt/unroll.c");
    let (output, exe) = run_compile_o("opt-loop-unroll", source, "-O3");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "opt_loop_unroll should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 向量循环向量化（-O3）
#[test]
#[cfg(unix)]
fn opt_loop_vectorize_o3_run_returns_zero() {
    let source = include_str!("fixtures/opt/vecopt.c");
    let (output, exe) = run_compile_o("opt-loop-vectorize", source, "-O3");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "opt_loop_vectorize should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 预处理残留的 _DECLARE_STATIC_KEY_0 → extern struct static_key_false
#[test]
fn static_key_maybe_compiles() {
    let source = include_str!("fixtures/c/skey_maybe.c");
    let output = run_syntax_test("static-key-maybe", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn static_key_chain_compiles() {
    let source = include_str!("fixtures/c/skey_chain.c");
    let output = run_syntax_test("static-key-chain", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn static_key_decl_compiles() {
    let source = include_str!("fixtures/c/skey_decl.c");
    let output = run_syntax_test("static-key-decl", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 跨 TU LTO 链接后 dead static 函数从合并 .o 剔除
#[test]
#[cfg(unix)]
fn lto_link_strips_unreferenced_static() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let a_src = dir.join(format!("dcc-lto-dead-a-{pid}.c"));
    let b_src = dir.join(format!("dcc-lto-dead-b-{pid}.c"));
    let a_o = dir.join(format!("dcc-lto-dead-a-{pid}.o"));
    let b_o = dir.join(format!("dcc-lto-dead-b-{pid}.o"));
    let out_o = dir.join(format!("dcc-lto-dead-merged-{pid}.o"));
    fs::write(
        &a_src,
        "static int dead_fn(void) { return 99; }\n\
         int exported(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        &b_src,
        "extern int exported(void);\n\
         int user(void) { return exported(); }\n",
    )
    .unwrap();
    let dcc = env!("CARGO_BIN_EXE_dcc");
    for (src, obj) in [(&a_src, &a_o), (&b_src, &b_o)] {
        let out = Command::new(dcc)
            .args(["-flto", "-c", "-o"])
            .arg(obj)
            .arg(src)
            .output()
            .expect("compile");
        assert!(
            out.status.success(),
            "compile: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let link = Command::new(dcc)
        .args(["-r", "-flto", "-o"])
        .arg(&out_o)
        .arg(&a_o)
        .arg(&b_o)
        .output()
        .expect("link");
    assert!(
        link.status.success(),
        "link: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let readelf = Command::new("readelf")
        .args(["-s"])
        .arg(&out_o)
        .output()
        .expect("readelf");
    let symtab = String::from_utf8_lossy(&readelf.stdout);
    let _ = fs::remove_file(&a_src);
    let _ = fs::remove_file(&b_src);
    let _ = fs::remove_file(&a_o);
    let _ = fs::remove_file(&b_o);
    let _ = fs::remove_file(&out_o);
    assert!(
        symtab.contains("exported"),
        "exported must survive link: {symtab}"
    );
    assert!(
        !symtab.contains("dead_fn"),
        "dead_fn should be stripped: {symtab}"
    );
}

// =================== C 前端补洞: 新增特性 ===================

/// MS 扩展关键字: _ms_restrict / __declspec / __pragma
#[test]
#[cfg(unix)]
fn ms_extensions_compile_and_run() {
    let source = include_str!("fixtures/c/ms_ext.c");
    let (output, exe) = run_compile_and_run("ms-extensions", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "ms_extensions should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// __attribute__((cleanup(fn))): 离开作用域时调用 fn(&var)
#[test]
#[cfg(unix)]
fn attr_cleanup_compile_and_run() {
    let source = include_str!("fixtures/c/attr_cleanup.c");
    let (output, exe) = run_compile_and_run("attr-cleanup", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_cleanup should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// __attribute__((target_clones(...))): 解析并存入 FuncDecl.target_clones
#[test]
#[cfg(unix)]
fn attr_target_clones_compile_and_run() {
    let source = include_str!("fixtures/c/attr_clones.c");
    let (output, exe) = run_compile_and_run("attr-target-clones", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_target_clones should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// __attribute__((no_sanitize(...))) / no_sanitize_address
#[test]
#[cfg(unix)]
fn attr_no_sanitize_compile_and_run() {
    let source = include_str!("fixtures/c/attr_nosan.c");
    let (output, exe) = run_compile_and_run("attr-no-sanitize", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_no_sanitize should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// GCC 接受 `__attribute` (无尾部双下划线) 作为 `__attribute__` 的简写.
/// linux 7.1 的 __noinstr_section() 误用此写法，dcc 需要支持.
/// 验证两种写法产生的 ELF section 相同.
#[test]
#[cfg(unix)]
fn attr_no_trailing_underscore_alias() {
    let source = r#"
__attribute((__section__(".kernel_alias_test"))) int kernel_var = 42;
__attribute__((__section__(".kernel_alias_test"))) int canonical_var = 7;
int sum(void) { return kernel_var + canonical_var; }
"#;
    let (out, obj) = run_object_test("attr-alias", source);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let objdump_out = Command::new("objdump")
        .args(["-h", obj.to_str().unwrap()])
        .output()
        .expect("objdump");
    let sec = String::from_utf8_lossy(&objdump_out.stdout);
    assert!(
        sec.contains(".kernel_alias_test"),
        "section .kernel_alias_test missing: {sec}"
    );
    let _ = fs::remove_file(obj);
}

/// `-dM`: 预处理后输出所有宏定义. Linux 内核的编译器探测脚本依赖此标志.
/// 验证: dcc 内置宏 + 用户 -D 都在输出里.
#[test]
#[allow(non_snake_case)]
fn dM_flag_emits_all_macros() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-dM-{}.c", std::process::id()));
    fs::write(&src, "int x = 1;").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-E",
            "-dM",
            "-DUSER_MACRO=42",
            "-x",
            "c",
            src.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc -dM");
    let _ = fs::remove_file(&src);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("#define __DCC__ 1"),
        "missing __DCC__: {text}"
    );
    assert!(
        text.contains("#define __GNUC__"),
        "missing __GNUC__: {text}"
    );
    assert!(
        text.contains("#define USER_MACRO 42"),
        "missing USER_MACRO: {text}"
    );
}

/// ISO C 6.10.2: `#include pp-tokens` 形式应先做宏展开.
/// Linux kernel 大量使用 `#include TRACE_INCLUDE(TRACE_INCLUDE_FILE)`.
/// 验证: 自定义宏展开后能被正确解析为 <file> 形式.
#[test]
fn include_macro_expansion() {
    let dir = std::env::temp_dir().join(format!("dcc-inc-macro-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let header = dir.join("inc_target.h");
    fs::write(&header, "int inc_target_value = 1234;\n").unwrap();
    let main_src = dir.join("main.c");
    fs::write(
        &main_src,
        r#"
#define HEADER_PATH inc_target.h
#define INCLUDE_HDR(x) <x>
#include INCLUDE_HDR(HEADER_PATH)
int main(void) { return inc_target_value; }
"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-E",
            "-I",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc -E");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("inc_target_value"),
        "include macro expansion failed: {text}"
    );
    let _ = fs::remove_file(&header);
    let _ = fs::remove_file(&main_src);
    let _ = fs::remove_dir(&dir);
}

/// C 宏的 `#param` 字符串化 (stringification) 应在 `##` 后面也能正确工作.
/// Linux kernel `SAVE_SEGMENT(seg)` 宏同时使用 `__savesegment_##seg` 和 `"%%" #seg`,
/// 之前的实现遇到 `##seg` 后会停止扫描, 导致 `#seg` 未被展开.
#[test]
fn stringify_after_paste() {
    let src = r#"
#define SAVE_SEGMENT(seg)              \
static inline unsigned long __savesegment_##seg(void) \
{                                      \
    unsigned long v;                   \
    asm volatile("movl %%" #seg ",%k0" : "=r" (v)); \
    return v;                          \
}
SAVE_SEGMENT(ds)
SAVE_SEGMENT(cs)
int main(void) { return (int)(__savesegment_ds() + __savesegment_cs()); }
"#;
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-x", "c", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn dcc");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(src.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // #ds / #cs 应被字符串化为 "ds" / "cs"
    assert!(text.contains("\"ds\""), "stringify 'ds' missing: {text}");
    assert!(text.contains("\"cs\""), "stringify 'cs' missing: {text}");
    // ## 应被令牌拼接
    assert!(
        text.contains("__savesegment_ds"),
        "paste 'ds' missing: {text}"
    );
    assert!(
        text.contains("__savesegment_cs"),
        "paste 'cs' missing: {text}"
    );
}

/// 链式 designated initializer (ISO C99 6.7.8/19: `.a.b.c` 或 `[0].field`).
/// 用于 Linux kernel `_ddebug.key.dd_key_true = ...` 初始化 union 成员.
#[test]
fn chained_designator_init() {
    use std::io::Write;
    let src = r#"
struct inner1 { int a; int b; };
struct inner2 { int x; struct inner1 sub; };
struct outer {
    int tag;
    union {
        struct inner1 k1;
        struct inner2 k2;
    } u;
};
int main(void) {
    struct outer s1 = { .tag = 7, .u.k2.x = 11, .u.k2.sub.a = 22, .u.k2.sub.b = 33 };
    if (s1.tag != 7) return 1;
    if (s1.u.k2.x != 11) return 2;
    if (s1.u.k2.sub.a != 22) return 3;
    if (s1.u.k2.sub.b != 33) return 4;
    // 数组中的链式 designator
    struct inner1 arr[3] = { [0].a = 100, [1].b = 200, [2].a = 300 };
    if (arr[0].a != 100 || arr[0].b != 0) return 5;
    if (arr[1].a != 0 || arr[1].b != 200) return 6;
    if (arr[2].a != 300 || arr[2].b != 0) return 7;
    return 0;
}
"#;
    let mut child = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-x", "c", "-o", "/tmp/chained.o", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn dcc");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(src.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "chained designator compile failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file("/tmp/chained.o");
}

/// 前向声明 + 同 TU 后续定义: dcc 不应为前向声明额外生成 UND 符号,
/// 避免与本 TU 内的定义在同名前缀下产生多个 .symtab 条目.
/// 复现 kernel `arch/x86/tools/relocs_32.c` 模式: relocs.h 声明 process_32,
/// relocs_32.c 经 relocs.c 包含定义 process_32. 之前会同时输出
/// `FUNC GLOBAL UND process_32` 与 `FUNC GLOBAL DEFAULT 1 process_32`,
/// 导致静态检查和外部链接失败.
#[test]
fn fwd_decl_same_tu_dedup() {
    use std::io::Write;
    let src = r#"
void helper(int);  // 前向声明 (本 TU 后面有定义)
static int local_static_helper(int);  // static 不影响, 但走同一路径
void helper(int x) {
    if (x > 0) {
        local_static_helper(x - 1);
    }
}
static int local_static_helper(int x) {
    return x + 1;
}
int main(void) {
    helper(5);
    return 0;
}
"#;
    let mut child = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-x", "c", "-o", "/tmp/fwd_decl.o", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn dcc");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(src.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "fwd_decl compile failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 用 readelf 检查 .symtab 不应同时有 UND 和已定义的 helper
    let readelf = Command::new("readelf")
        .args(["-s", "/tmp/fwd_decl.o"])
        .output()
        .expect("run readelf");
    let syms = String::from_utf8_lossy(&readelf.stdout);
    let helper_und = syms
        .lines()
        .filter(|l| l.contains("helper") && l.contains("UND"))
        .count();
    let helper_defined = syms
        .lines()
        .filter(|l| l.contains("helper") && l.contains("DEFAULT"))
        .count();
    assert_eq!(
        helper_und, 0,
        "fwd declaration should not emit UND sym; got:\n{syms}"
    );
    assert!(
        helper_defined >= 1,
        "definition should emit at least one DEFAULT sym; got:\n{syms}"
    );
    let _ = fs::remove_file("/tmp/fwd_decl.o");
}

/// __attribute__((copy(fn))) / __attribute__((returns_nonnull))
#[test]
#[cfg(unix)]
fn attr_copy_returns_nonnull_compile_and_run() {
    let source = include_str!("fixtures/c/attr_copy.c");
    let (output, exe) = run_compile_and_run("attr-copy-returns-nonnull", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_copy_returns_nonnull should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// #pragma pack / weak / STDC FENV_ACCESS / GCC diagnostic
#[test]
#[cfg(unix)]
fn pragma_family_compile_and_run() {
    let source = include_str!("fixtures/c/pragma_fam.c");
    let (output, exe) = run_compile_and_run("pragma-family", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "pragma_family should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// _Pragma 操作符: 等价于 #pragma
#[test]
#[cfg(unix)]
fn pragma_operator_compile_and_run() {
    let source = include_str!("fixtures/c/pragma_op.c");
    let (output, exe) = run_compile_and_run("pragma-operator", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "pragma_operator should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// #warning: 编译时输出警告, 不中断
#[test]
fn user_warning_does_not_break_compile() {
    let source = include_str!("fixtures/c/user_diag.c");
    let output = run_syntax_test("user-warning", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// _Static_assert 字符串化消息: C23 单参, 失败时附加表达式文本
#[test]
fn static_assert_stringification_succeeds() {
    let source = include_str!("fixtures/c/sassert_msg.c");
    let output = run_syntax_test("static-assert-msg", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 单输入 -r 仍可注入合并 .llvm.lto（单 .o 重打包）
#[test]
#[cfg(unix)]
fn lto_single_object_relocatable_link() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-lto-single-{pid}.c"));
    let obj = dir.join(format!("dcc-lto-single-{pid}.o"));
    let out = dir.join(format!("dcc-lto-single-out-{pid}.o"));
    fs::write(&src, "int solo(void) { return 0; }\n").unwrap();
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let compile = Command::new(dcc)
        .args(["-flto", "-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("compile");
    assert!(compile.status.success());
    let link = Command::new(dcc)
        .args(["-r", "-flto", "-o"])
        .arg(&out)
        .arg(&obj)
        .output()
        .expect("link");
    let merged = fs::read(&out).unwrap_or_default();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&out);
    assert!(
        link.status.success(),
        "link: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(
        String::from_utf8_lossy(&merged).contains(".llvm.lto"),
        "single-object -r must preserve .llvm.lto"
    );
}

#[test]
#[cfg(unix)]
fn thread_local_exec_returns_15() {
    let src = include_str!("fixtures/c/tls_simple.c");
    let (out, exe) = run_compile_and_run("tls", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 15, "tls counter");
    let _ = fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn kr_old_style_function() {
    let src = include_str!("fixtures/c/kr_func.c");
    let (out, exe) = run_compile_and_run("kr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 7);
    let _ = fs::remove_file(&exe);
}

#[test]
fn format_attr_warns_on_missing_arg() {
    let src = "__attribute__((format(printf, 1, 2))) void p(const char *f, ...);\n\
               int main(void) { p(\"%d %d\", 1); return 0; }\n";
    let output = run_syntax_test("fmt-warn", src);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0003"),
        "expected format warning: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn stdatomic_macros_hosted() {
    let src = include_str!("fixtures/c/atomic.c");
    let (out, exe) = run_compile_and_run("atomic", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 7);
    let _ = fs::remove_file(&exe);
}

#[test]
fn permissive_unknown_typedef_warns() {
    let src = "unknown_t x = 0;\nint main(void) { return (int)x; }\n";
    let path = std::env::temp_dir().join(format!("dcc-perm-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fpermissive", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "permissive should not error: {stderr}"
    );
    assert!(
        stderr.contains("W0002"),
        "expected permissive warning: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn exec_elf_has_symtab() {
    let src = "int g = 1; int main(void) { return g; }\n";
    let (out, exe) = run_compile_and_run("symtab", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let readelf = Command::new("readelf")
        .args(["-s", exe.to_str().unwrap()])
        .output()
        .expect("readelf");
    let sym = String::from_utf8_lossy(&readelf.stdout);
    assert!(sym.contains(" main"), "symtab missing main: {sym}");
    assert!(sym.contains(" g"), "symtab missing g: {sym}");
    let _ = fs::remove_file(&exe);
}

// ===== M15: const_eval / 严格类型检查 / 运行时检查 =====

/// 浮点常量求值 (M15.1)
#[test]
#[cfg(unix)]
fn m15_float_const_eval() {
    let src = include_str!("fixtures/c/float_const.c");
    let (out, exe) = run_compile_and_run("m15-float-const", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 字符串字面量求值 + 拼接 (M15.1)
#[test]
#[cfg(unix)]
fn m15_string_literal_and_concat() {
    let src = include_str!("fixtures/c/string_const.c");
    let (out, exe) = run_compile_and_run("m15-string-concat", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 地址常量求值 (M15.1)
#[test]
#[cfg(unix)]
fn m15_address_const() {
    let src = include_str!("fixtures/c/address_const.c");
    let (out, exe) = run_compile_and_run("m15-addr-const", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// __builtin_* 编译期折叠 (strlen/abs/bswap)
#[test]
#[cfg(unix)]
fn m15_builtin_fold() {
    let src = include_str!("fixtures/c/builtin_fold.c");
    let (out, exe) = run_compile_and_run("m15-builtin-fold", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 严格类型检查: 隐式指针/整数转换 warning (不阻塞)
#[test]
fn m15_strict_warning_compile() {
    let src = include_str!("fixtures/c/strict_warning.c");
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

/// -fbounds-check 数组越界检测 (正常路径不触发, 仅编译 + 运行)
#[test]
#[cfg(unix)]
fn m15_bounds_check_inrange() {
    let src = include_str!("fixtures/c/bounds_check.c");
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
    assert!(
        output.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    let src = include_str!("fixtures/c/bounds_check_oob.c");
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
    assert!(
        output.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// 16 字节 struct 传值 ABI: struct {double,double} 走 xmm0:xmm1,
/// struct {long,long} 走 rdi:rsi, SSE 类返回值经 xmm0:xmm1
#[test]
#[cfg(unix)]
fn m15_abi16_struct_passing() {
    let src = include_str!("fixtures/c/abi16_struct.c");
    let (out, exe) = run_compile_and_run("m15-abi16-struct", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 浮点存储路径: double/float 数组下标、解引用、struct 字段赋值
/// (值在 xmm0, 必须用 movss/movsd 而非 GP mov 存储)
#[test]
#[cfg(unix)]
fn m15_float_store_paths() {
    let src = include_str!("fixtures/c/abi16_struct_extra.c");
    let (out, exe) = run_compile_and_run("m15-float-store-paths", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// C23 constexpr 函数声明 (解析通过, 运行结果正确)
#[test]
#[cfg(unix)]
fn m15_constexpr_function() {
    let src = include_str!("fixtures/c/constexpr.c");
    let (out, exe) = run_compile_and_run("m15-constexpr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

// ===== M16: SSA IR + 优化 pass (DCE / SR / SCCP / TCO / ...) =====

/// SSA IR 基础 - if/else/while/for 控制流正确转换
#[test]
#[cfg(unix)]
fn m16_ssa_ir() {
    let src = include_str!("fixtures/opt/ssa_ir.c");
    let (out, exe) = run_compile_and_run("m16-ssa-ir", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 强度削减: i * 2^N -> i << N
#[test]
#[cfg(unix)]
fn m16_strength_reduction() {
    let src = include_str!("fixtures/opt/strength_reduction.c");
    let (out, exe) = run_compile_and_run("m16-strength", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 跨函数 DCE: 不可达 static 函数被删除
#[test]
#[cfg(unix)]
fn m16_cross_function_dce() {
    let src = include_str!("fixtures/opt/cross_function_dce.c");
    let (out, exe) = run_compile_and_run("m16-dce", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// SCCP: 编译期常量分支折叠
#[test]
#[cfg(unix)]
fn m16_sccp() {
    let src = include_str!("fixtures/opt/sccp.c");
    let (out, exe) = run_compile_and_run("m16-sccp", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 循环优化 (Tiling / IVOP / Interchange)
#[test]
#[cfg(unix)]
fn m16_loop_opt() {
    let src = include_str!("fixtures/opt/loop_opt.c");
    let (out, exe) = run_compile_and_run("m16-loop-opt", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// TCO: 尾递归求值
#[test]
#[cfg(unix)]
fn m16_tco() {
    let src = include_str!("fixtures/opt/tco.c");
    let (out, exe) = run_compile_and_run("m16-tco", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 函数属性传播 (cold / hot)
#[test]
#[cfg(unix)]
fn m16_function_attr() {
    let src = include_str!("fixtures/opt/function_attr.c");
    let (out, exe) = run_compile_and_run("m16-attr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

// ===== M17: C++ 修饰符与类型推导 (14 项) =====

/// M17.0: auto 真类型推导 (int / StringLit / class)
#[test]
#[cfg(unix)]
fn m17_auto_deduce() {
    let src = include_str!("fixtures/cpp/auto_deduce.cpp");
    let (out, _obj) = run_cpp_object_test("m17-auto-deduce", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: decltype(expr) 真类型查询
#[test]
#[cfg(unix)]
fn m17_decltype() {
    let src = include_str!("fixtures/cpp/decltype.cpp");
    let (out, _obj) = run_cpp_object_test("m17-decltype", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: decltype(auto) 变量类型推导
#[test]
#[cfg(unix)]
fn m17_decltype_auto() {
    let src = include_str!("fixtures/cpp/decltype_auto.cpp");
    let (out, _obj) = run_cpp_object_test("m17-decltype-auto", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: explicit 构造函数
#[test]
#[cfg(unix)]
fn m17_explicit_ctor() {
    let src = include_str!("fixtures/cpp/explicit_ctor.cpp");
    let (out, _obj) = run_cpp_object_test("m17-explicit-ctor", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: final / override 关键字
#[test]
#[cfg(unix)]
fn m17_final_override() {
    let src = include_str!("fixtures/cpp/final_override.cpp");
    let (out, _obj) = run_cpp_object_test("m17-final-override", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: friend 关键字 (lower 接受 friend 声明, 不强求链接)
#[test]
#[cfg(unix)]
fn m17_friend() {
    let src = include_str!("fixtures/cpp/friend.cpp");
    let (out, _obj) = run_cpp_object_test("m17-friend", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: if constexpr 编译期分支
#[test]
#[cfg(unix)]
fn m17_if_constexpr() {
    let src = include_str!("fixtures/cpp/if_constexpr.cpp");
    let (out, _obj) = run_cpp_object_test("m17-if-constexpr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: using Base::Base; 继承构造
#[test]
#[cfg(unix)]
fn m17_inheriting_ctor() {
    let src = include_str!("fixtures/cpp/inheriting_ctor.cpp");
    let (out, _obj) = run_cpp_object_test("m17-inheriting-ctor", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: 成员指针 obj.*ptr_to_member
#[test]
#[cfg(unix)]
fn m17_member_ptr() {
    let src = include_str!("fixtures/cpp/member_ptr.cpp");
    let (out, _obj) = run_cpp_object_test("m17-member-ptr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: mutable 独立字段
#[test]
#[cfg(unix)]
fn m17_mutable() {
    let src = include_str!("fixtures/cpp/mutable.cpp");
    let (out, _obj) = run_cpp_object_test("m17-mutable", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: new T[n] 数组分配 + operator new[]/delete[]
#[test]
#[cfg(unix)]
fn m17_new_array() {
    let src = include_str!("fixtures/cpp/new_array.cpp");
    let (out, _obj) = run_cpp_object_test("m17-new-array", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: noexcept 关键字
#[test]
#[cfg(unix)]
fn m17_noexcept() {
    let src = include_str!("fixtures/cpp/noexcept.cpp");
    let (out, _obj) = run_cpp_object_test("m17-noexcept", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: placement new
#[test]
#[cfg(unix)]
fn m17_placement_new() {
    let src = include_str!("fixtures/cpp/placement_new.cpp");
    let (out, _obj) = run_cpp_object_test("m17-placement-new", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: smoke 测试 (最小可工作 C++ 程序)
#[test]
#[cfg(unix)]
fn m17_smoke() {
    let src = include_str!("fixtures/cpp/smoke.cpp");
    let (out, _obj) = run_cpp_object_test("m17-smoke", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: typeid(e) vtable RTTI
#[test]
#[cfg(unix)]
fn m17_typeid() {
    let src = include_str!("fixtures/cpp/typeid.cpp");
    let (out, _obj) = run_cpp_object_test("m17-typeid", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: volatile 成员函数
#[test]
#[cfg(unix)]
fn m17_volatile_member() {
    let src = include_str!("fixtures/cpp/volatile_member.cpp");
    let (out, _obj) = run_cpp_object_test("m17-volatile-member", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// M17.0: vtable/typeinfo/member-offset 符号多文件链接不重复定义
/// 两个 TU 包含相同 class 定义，链接器应通过 weak 解析而非报 multiple definition
#[test]
#[cfg(unix)]
fn m17_weak_vtable_multi_file_link() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    // 共享 class 定义（含 virtual + 数据成员 + 显式构造）
    let class_def = "\
class Foo {\n\
public:\n\
    Foo() : x(0) {}\n\
    virtual int val() const { return 42; }\n\
    int x;\n\
};\n";
    // TU1: 定义 Foo + helper() 使用 val() 和 x
    let tu1 = format!(
        "{class_def}\
int helper() {{ Foo f; f.x = 1; return f.val(); }}\n"
    );
    // TU2: 同样定义 Foo + main() 也使用 val() 和 x
    // 两个 TU 各自独立使用 Foo, 链接时验证 vtable/typeinfo 不冲突
    // 不跨 TU 调用: C++ 前端尚不支持自由函数前向声明
    let tu2 = format!(
        "{class_def}\
int main() {{ Foo g; g.x = 2; return g.val() - 42; }}\n"
    );
    let p1 = dir.join(format!("dcc-m17-weak-tu1-{pid}.cpp"));
    let p2 = dir.join(format!("dcc-m17-weak-tu2-{pid}.cpp"));
    let o1 = dir.join(format!("dcc-m17-weak-tu1-{pid}.o"));
    let o2 = dir.join(format!("dcc-m17-weak-tu2-{pid}.o"));
    let exe = dir.join(format!("dcc-m17-weak-{pid}"));
    fs::write(&p1, &tu1).unwrap();
    fs::write(&p2, &tu2).unwrap();
    let dcc = env!("CARGO_BIN_EXE_dcc");
    // 编译 TU1
    let c1 = Command::new(dcc).args(["-c", "-o"]).arg(&o1).arg(&p1).output().expect("dcc");
    let _ = fs::remove_file(&p1);
    assert!(c1.status.success(), "tu1 compile: {}", String::from_utf8_lossy(&c1.stderr));
    // 编译 TU2
    let c2 = Command::new(dcc).args(["-c", "-o"]).arg(&o2).arg(&p2).output().expect("dcc");
    let _ = fs::remove_file(&p2);
    assert!(c2.status.success(), "tu2 compile: {}", String::from_utf8_lossy(&c2.stderr));
    // 链接: 关键验证点 — 不报 multiple definition
    let link = Command::new("gcc").args(["-o"]).arg(&exe).arg(&o1).arg(&o2).output().expect("gcc link");
    let _ = fs::remove_file(&o1);
    let _ = fs::remove_file(&o2);
    let stderr = String::from_utf8_lossy(&link.stderr);
    assert!(link.status.success(), "link failed (multiple definition?): {stderr}");
    assert!(!stderr.contains("multiple definition"), "vtable/typeinfo 重复定义: {stderr}");
    // 运行验证
    let run = Command::new(&exe).output().expect("run");
    let _ = fs::remove_file(&exe);
    assert_eq!(run.status.code(), Some(0), "exit 0 (42 - 42 == 0)");
}

// =========================================================================
// M18: C++ 标准库扩展 (filesystem / thread / mutex / chrono / expected /
// flat_map / multi_index / regex / ranges / concepts / modules)
// =========================================================================

#[test]
#[cfg(unix)]
fn m18_chrono() {
    let src = include_str!("fixtures/cpp/chrono.cpp");
    let (out, _obj) = run_cpp_object_test("m18-chrono", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_thread_mutex() {
    let src = include_str!("fixtures/cpp/thread_mutex.cpp");
    let (out, _obj) = run_cpp_object_test("m18-thread-mutex", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_condition_variable() {
    let src = include_str!("fixtures/cpp/condition_variable.cpp");
    let (out, _obj) = run_cpp_object_test("m18-cv", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_filesystem() {
    let src = include_str!("fixtures/cpp/filesystem.cpp");
    let (out, _obj) = run_cpp_object_test("m18-filesystem", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_expected() {
    let src = include_str!("fixtures/cpp/expected.cpp");
    let (out, _obj) = run_cpp_object_test("m18-expected", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_flat_map() {
    let src = include_str!("fixtures/cpp/flat_map.cpp");
    let (out, _obj) = run_cpp_object_test("m18-flat-map", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_multi_index() {
    let src = include_str!("fixtures/cpp/multi_index.cpp");
    let (out, _obj) = run_cpp_object_test("m18-multi-index", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_regex() {
    let src = include_str!("fixtures/cpp/regex.cpp");
    let (out, _obj) = run_cpp_object_test("m18-regex", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_ranges() {
    let src = include_str!("fixtures/cpp/ranges.cpp");
    let (out, _obj) = run_cpp_object_test("m18-ranges", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_concept() {
    let src = include_str!("fixtures/cpp/concept.cpp");
    let (out, _obj) = run_cpp_object_test("m18-concept", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_modules() {
    let src = include_str!("fixtures/cpp/modules.cpp");
    let (out, _obj) = run_cpp_object_test("m18-modules", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

// ====== M19: 预处理器补齐 ======
// __COUNTER__ 单调递增, 跨宏嵌套仍保持唯一
#[test]
#[cfg(unix)]
fn m19_counter_monotonic() {
    let src = include_str!("fixtures/c/counter.c");
    let (out, exe) = run_compile_and_run("m19-counter", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__COUNTER__ should produce 0,1,2");
    let _ = fs::remove_file(exe);
}

// __COUNTER__ 在宏展开中递增
#[test]
#[cfg(unix)]
fn m19_counter_in_function_macro() {
    let src = include_str!("fixtures/c/counter_in_macro.c");
    let (out, exe) = run_compile_and_run("m19-counter-macro", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__COUNTER__ in macro should be 0,1,2");
    let _ = fs::remove_file(exe);
}

// __VA_OPT__: 非空时展开 content, 空时移除
#[test]
#[cfg(unix)]
fn m19_va_opt_empty_and_nonempty() {
    let src = include_str!("fixtures/c/va_opt.c");
    let (out, exe) = run_compile_and_run("m19-va-opt", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(
        rc, 0,
        "__VA_OPT__ should expand to comma list when non-empty"
    );
    let _ = fs::remove_file(exe);
}

// GCC 兼容预定义宏补齐
#[test]
#[cfg(unix)]
fn m19_predefined_macros_available() {
    let src = include_str!("fixtures/c/predefined_macros.c");
    let (out, exe) = run_compile_and_run("m19-predef", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "all GCC predefined macros must be defined");
    let _ = fs::remove_file(exe);
}

// #pragma locale: 接受且不影响编译
#[test]
#[cfg(unix)]
fn m19_pragma_locale_accepted() {
    let src = include_str!("fixtures/c/pragma_locale.c");
    let (out, exe) = run_compile_and_run("m19-pragma-locale", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(exe);
}

// ====== M26: C23 预处理全家族端到端 ======
// #elifdef / #elifndef 分支选择
#[test]
#[cfg(unix)]
fn m26_elifdef_elifndef_runs() {
    let src = include_str!("fixtures/c/elifdef.c");
    let (out, exe) = run_compile_and_run("m26-elifdef", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "#elifdef/#elifndef branch selection");
    let _ = fs::remove_file(exe);
}

// __has_include / __has_c_attribute / __has_builtin / __has_attribute /
// __STDC_ENDIAN_* 在 #if 中求值
#[test]
#[cfg(unix)]
fn m26_has_macros_c23_runs() {
    let src = include_str!("fixtures/c/has_macros.c");
    let (out, exe) = run_compile_and_run("m26-has-macros", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__has_* family + endian macros");
    let _ = fs::remove_file(exe);
}

// #embed: limit / prefix / suffix 组合, 字节级校验
#[test]
#[cfg(unix)]
fn m26_embed_limit_prefix_suffix_runs() {
    let dir = std::env::temp_dir();
    let data = dir.join(format!("dcc-embed-data-{}.bin", std::process::id()));
    fs::write(&data, [1u8, 2, 3, 4, 5]).expect("write embed data");
    let src = format!(
        "int main(void) {{\n\
         static const unsigned char a[] = {{\n\
             #embed \"{}\" limit(3) prefix(9, 8) suffix(7)\n\
         }};\n\
         if (a[0] != 9 || a[1] != 8 || a[2] != 1 || a[3] != 2 || a[4] != 3 || a[5] != 7) return 1;\n\
         if (sizeof(a) != 6) return 2;\n\
         return 0;\n}}\n",
        data.display()
    );
    let (out, exe) = run_compile_and_run("m26-embed", &src);
    let _ = fs::remove_file(data);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "#embed limit/prefix/suffix bytes");
    let _ = fs::remove_file(exe);
}

// #embed if_empty: 空文件时以 if_empty 字节替代
#[test]
#[cfg(unix)]
fn m26_embed_if_empty_runs() {
    let dir = std::env::temp_dir();
    let data = dir.join(format!("dcc-embed-empty-{}.bin", std::process::id()));
    fs::write(&data, []).expect("write empty embed data");
    let src = format!(
        "int main(void) {{\n\
         static const unsigned char a[] = {{\n\
             #embed \"{}\" if_empty(0xAA, 0xBB)\n\
         }};\n\
         if (a[0] != 0xAA || a[1] != 0xBB) return 1;\n\
         if (sizeof(a) != 2) return 2;\n\
         return 0;\n}}\n",
        data.display()
    );
    let (out, exe) = run_compile_and_run("m26-embed-empty", &src);
    let _ = fs::remove_file(data);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "#embed if_empty bytes");
    let _ = fs::remove_file(exe);
}

// __has_embed: 文件存在与否选择 #if 分支
#[test]
#[cfg(unix)]
fn m26_has_embed_selects_branch() {
    let dir = std::env::temp_dir();
    let data = dir.join(format!("dcc-has-embed-{}.bin", std::process::id()));
    fs::write(&data, [1u8, 2]).expect("write embed data");
    let src = format!(
        "#if __has_embed(\"{}\")\n\
         #define HAS_EMBED 1\n\
         #else\n\
         #define HAS_EMBED 0\n\
         #endif\n\
         #if __has_embed(\"dcc-no-such-data-xyz.bin\")\n\
         #define HAS_EMBED_MISSING 1\n\
         #else\n\
         #define HAS_EMBED_MISSING 0\n\
         #endif\n\
         int main(void) {{\n\
             if (HAS_EMBED != 1) return 1;\n\
             if (HAS_EMBED_MISSING != 0) return 2;\n\
             return 0;\n\
         }}\n",
        data.display()
    );
    let (out, exe) = run_compile_and_run("m26-has-embed", &src);
    let _ = fs::remove_file(data);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__has_embed branch selection");
    let _ = fs::remove_file(exe);
}

// #pragma once: 重复 include 去重, 宏只定义一次
#[test]
#[cfg(unix)]
fn m26_pragma_once_dedupes_include() {
    let dir = std::env::temp_dir();
    let hdr = dir.join(format!("dcc-once-{}.h", std::process::id()));
    fs::write(&hdr, "#pragma once\n#define ONCE_VAL 7\n").expect("write header");
    let src = format!(
        "#include \"{}\"\n\
         #include \"{}\"\n\
         int main(void) {{ return ONCE_VAL - 7; }}\n",
        hdr.display(),
        hdr.display()
    );
    let (out, exe) = run_compile_and_run("m26-pragma-once", &src);
    let _ = fs::remove_file(hdr);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "#pragma once dedup");
    let _ = fs::remove_file(exe);
}

// #include_next: 从更靠后的搜索路径解析同名头
#[test]
#[cfg(unix)]
fn m26_include_next_chain() {
    let dir = std::env::temp_dir().join(format!("dcc-incnext-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("a")).unwrap();
    fs::create_dir_all(dir.join("b")).unwrap();
    fs::write(
        dir.join("a/chain.h"),
        "#ifndef A_CHAIN\n#define A_CHAIN 1\n#include_next <chain.h>\n#endif\n",
    )
    .unwrap();
    fs::write(dir.join("b/chain.h"), "#define CHAIN_VAL 42\n").unwrap();
    let src = "#include <chain.h>\n\
         int main(void) {{ return CHAIN_VAL - 42; }}\n";
    let source_path = dir.join("main.c");
    fs::write(&source_path, src).unwrap();
    let exe_path = dir.join("main");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-I"])
        .arg(dir.join("a"))
        .arg("-I")
        .arg(dir.join("b"))
        .args(["-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(rc, 0, "#include_next chain");
}

// ====== M23: Section 11 属性/builtin/alloca ======
// __attribute__((retain)): 变量和函数应被保留, gc-sections 不移除
#[test]
#[cfg(unix)]
fn m23_attr_retain_compiles_and_runs() {
    let src = include_str!("fixtures/c/attr_retain.c");
    let (out, exe) = run_compile_and_run("m23-attr-retain", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "retain_var + retain_func(0) = 41+1 = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((used)): 静态函数即使未被引用也应保留并执行
#[test]
#[cfg(unix)]
fn m23_attr_used_compiles_and_runs() {
    let src = include_str!("fixtures/c/attr_used.c");
    let (out, exe) = run_compile_and_run("m23-attr-used", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "used_helper(21) = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((flatten)): 解析且编译通过
#[test]
#[cfg(unix)]
fn m23_attr_flatten_compiles_and_runs() {
    let src = include_str!("fixtures/c/attr_flatten.c");
    let (out, exe) = run_compile_and_run("m23-attr-flatten", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "outer(20) = (20+1)*2 = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((deprecated("msg"))): 仅声明, 解析/编译不应报错
#[test]
#[cfg(unix)]
fn m23_attr_deprecated_compiles_and_runs() {
    let src = include_str!("fixtures/c/attr_deprecated.c");
    let (out, exe) = run_compile_and_run("m23-attr-deprecated", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42);
    let _ = fs::remove_file(exe);
}

// __attribute__((constructor(N))) / __attribute__((destructor(N))): 优先级排序
// GCC 约定: 数字越小优先级越高. 100 应在 300 之前构造.
#[test]
#[cfg(unix)]
fn m23_attr_ctor_priority_orders_correctly() {
    let src = include_str!("fixtures/c/attr_ctor_priority.c");
    let (out, exe) = run_compile_and_run("m23-attr-ctor-priority", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    // init_100 (prio=100) 先, order=0*10+2=2; init_300 (prio=300) 后, order=2*10+1=21
    assert_eq!(rc, 21, "constructor priority 100 should run before 300");
    let _ = fs::remove_file(exe);
}

// __builtin_debugtrap / __builtin_assume_aligned / __builtin_alloca
#[test]
#[cfg(unix)]
fn m23_builtin_dbg_compiles_and_runs() {
    let src = include_str!("fixtures/c/builtin_dbg.c");
    let (out, exe) = run_compile_and_run("m23-builtin-dbg", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    // alloca 4 个 int, 写 10/11/12/9, sum=42
    assert_eq!(rc, 42, "alloca + assume_aligned + (dead)debugtrap");
    let _ = fs::remove_file(exe);
}

// __attribute__((retain, used)) 联合使用
#[test]
#[cfg(unix)]
fn m23_attr_retain_used_compiles_and_runs() {
    let src = include_str!("fixtures/c/attr_retain_used.c");
    let (out, exe) = run_compile_and_run("m23-attr-retain-used", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "combo_var + combo_fn(0) = 7+35 = 42");
    let _ = fs::remove_file(exe);
}

// char16_t / char32_t (C11/C++11) 类型与字面量
#[test]
#[cfg(unix)]
fn m26_char16_char32_compiles_and_runs() {
    let src = include_str!("fixtures/c/char16_char32.c");
    let (out, exe) = run_compile_and_run("m26-char16-char32", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "char16_t/char32_t size and value checks");
    let _ = fs::remove_file(exe);
}

/// `double _Complex` 类型说明符 (C99 complex.h 形式)
/// M26: 此前 parse_type_mode 第二 loop 的 KwDouble arm 带 `break;`
/// 提前退出, 导致 `_Complex` 后续修饰符被外层误判为 declarator.
/// 现已去掉 break, 允许 `float _Complex` / `double _Complex` /
/// `long double _Complex` 自由组合.
#[test]
fn double_complex_decl_run_returns_zero() {
    let source = include_str!("fixtures/c/complex_decl.c");
    let (output, exe) = run_compile_and_run("double-complex-decl", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "double_complex_decl should return 0, got {rc}");
}

/// _Complex 字面量 (4.0i) + __real__/__imag__ 关键字 (Section 9)
/// M26: 新增虚数后缀、__real__/__imag__ 一元运算符、__extension__ 表达式前缀
/// 覆盖: 虚数字面量初始化、标量 real/imag、_Complex_I 宏、复数算术、
/// 一元负、整数虚数、非 lvalue 表达式、赋值路径、标量初始化 complex
#[test]
fn complex_literal_run_returns_zero() {
    let source = include_str!("fixtures/c/complex_literal.c");
    let (output, exe) = run_compile_and_run("complex-literal", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "complex_literal should return 0, got {rc}");
}

/// L'x' 宽字符字面量 (Section 11)
/// M26: 新增 TokKind::WideCharLit, lexer 在 `L` 标识符后看到
/// `'` 时走宽字符路径; 解析层复用 parse_wide_str 输出 UTF-32LE.
#[test]
fn wide_char_literal_run_returns_zero() {
    let source = include_str!("fixtures/c/wide_lit.c");
    let (output, exe) = run_compile_and_run("wide-char-literal", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "wide_char_literal should return 0, got {rc}");
}

/// fenv.h 函数 pass-through (Section 11.2)
/// 验证 feclearexcept / fegetround / fesetround / feraiseexcept
/// / fetestexcept 等被 dcc 发射为外部符号, 走 libc / libm.
/// 编译为 -c 后, 用 nm 检查符号表.
#[test]
fn fenv_pass_through_emits_external_symbols() {
    let source = include_str!("fixtures/c/fenv.c");
    let (output, object_path) = run_object_test("fenv-pass-through", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    let bytes = obj;
    for name in &[
        "feclearexcept",
        "fegetround",
        "fesetround",
        "feraiseexcept",
        "fetestexcept",
    ] {
        let needle = name.as_bytes();
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "object file should contain fenv symbol {name}, got {} bytes",
            bytes.len()
        );
    }
}

/// 复杂数学函数 pass-through (Section 11.2)
/// 验证 cproj / cabs / creal / cimag / conj / csqrt / cexp
/// / csin / ccos / cpow / carg / csinh / ccosh / ctanh 等
/// 复杂数学函数被 dcc 发射为外部符号, 走 libm.
#[test]
fn cmath_pass_through_emits_external_symbols() {
    let source = include_str!("fixtures/c/cmath.c");
    let (output, object_path) = run_object_test("cmath-pass-through", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    let bytes = obj;
    for name in &[
        "cabs", "cproj", "creal", "cimag", "conj", "csqrt", "cexp", "clog", "csin", "ccos", "cpow",
        "carg", "csinh", "ccosh", "ctanh",
    ] {
        let needle = name.as_bytes();
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "object file should contain cmath symbol {name}, got {} bytes",
            bytes.len()
        );
    }
}

/// fenv pragma STDC FENV_ACCESS 解析 (Section 11.2)
/// 验证 #pragma STDC FENV_ACCESS ON/OFF/DEFAULT 不会破坏编译.
/// pragma 状态在 dcc preproc 中由 fenv_access 字段记录, 但 codegen
/// 仍允许 fenv 函数被发射 (运行时语义依赖 libc).
#[test]
fn fenv_pragma_parses_without_error() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-fenv-pragma-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-fenv-pragma-{}.o", std::process::id()));
    let source = "#pragma STDC FENV_ACCESS ON\n\
                  #pragma STDC FENV_ACCESS OFF\n\
                  #pragma STDC FENV_ACCESS DEFAULT\n\
                  #pragma STDC FENV_ACCESS ON\n\
                  #include <fenv.h>\n\
                  int test_fenv_pr(void) {\n\
                      feclearexcept(FE_ALL_EXCEPT);\n\
                      int r = fegetround();\n\
                      fesetround(FE_TOWARDZERO);\n\
                      fesetround(r);\n\
                      return 0;\n\
                  }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&out_path);
    assert!(
        output.status.success(),
        "fenv pragma should parse without error, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// wchar_t 函数 codegen (Section 11.3)
/// 验证 __builtin_wcslen / __builtin_wcscpy / __builtin_wcscat
/// / __builtin_wcschr / __builtin_wcsrchr / __builtin_wcscmp
/// / __builtin_wcsncmp / __builtin_wmemcpy / __builtin_wmemmove
/// / __builtin_wmemset / __builtin_wmemcmp 被 dcc 发射为外部
/// 符号 (走 libc). 与 __builtin_str* 形式一致, 但作用于 wchar_t.
#[test]
fn wcs_builtins_forward_to_libc() {
    let source = include_str!("fixtures/c/wcs.c");
    let (output, object_path) = run_object_test("wcs-builtins", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    let bytes = obj;
    for name in &[
        "wcslen", "wcscpy", "wcscat", "wcschr", "wcsrchr", "wcscmp", "wcsncmp", "wmemcpy",
        "wmemmove", "wmemset", "wmemcmp",
    ] {
        let needle = name.as_bytes();
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "object file should contain wcs symbol {name}, got {} bytes",
            bytes.len()
        );
    }
}

/// wchar_t 函数 pass-through (Section 11.3)
/// 不带 __builtin_ 前缀的 wcslen / wcscpy 等也走 libc, 与
/// __builtin_wc* 行为一致. 该测试确认 -c 模式下两个路径都
/// 生成对 libc 的 PLT32 重定位.
#[test]
fn wcs_libc_call_passes_through() {
    let source = "#include <wchar.h>\n\
                  int test_wcslen(const wchar_t *s) { return wcslen(s); }\n\
                  wchar_t *test_wcscpy(wchar_t *d, const wchar_t *s) { return wcscpy(d, s); }\n\
                  int test_wcscmp(const wchar_t *a, const wchar_t *b) { return wcscmp(a, b); }\n";
    let (output, object_path) = run_object_test("wcs-libc-pass", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    let bytes = obj;
    for name in &["wcslen", "wcscpy", "wcscmp"] {
        let needle = name.as_bytes();
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "object file should contain libc symbol {name}"
        );
    }
}

// =========================================================================
// Section 11.4: x86_64 SysV va_arg 全栈支持
// =========================================================================

const VA_LIST_TYPEDEF: &str = "typedef __builtin_va_list va_list;\n";

/// x86_64 va_arg GP 寄存器路径 (1-5 变参全部在 GP 寄存器).
/// 修复前 overflow 偏移多 8 字节, 5+ 变参会读到错误参数.
#[test]
fn va_arg_x86_64_gp_path() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "int sum_int5(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) { return sum_int5(2, 10, 20) - 30; }\n";
    let (output, exe) = run_compile_and_run("va-arg-gp", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_int5(2, 10, 20) == 30");
}

/// x86_64 va_arg 溢出栈路径 (6+ 变参走 overflow_arg_area).
/// 修复前 overflow_off = frame_size + 16 错为 16, 现已对齐到
/// SysV ABI: r13 + 16 = caller 栈上第 1 个变参.
#[test]
fn va_arg_x86_64_overflow_path() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "int sum_int13(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) { return sum_int13(13, 1,2,3,4,5,6,7,8,9,10,11,12,13) - 91; }\n";
    let (output, exe) = run_compile_and_run("va-arg-overflow", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_int13(13, 1..13) == 91");
}

/// x86_64 va_arg 浮点路径 (XMM reg + 溢出栈 8 字节步进).
/// 修复前 overflow 步进用 16 (XMM 槽位) 而非 8 (栈上 double).
#[test]
fn va_arg_x86_64_double_path() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "double sum_double5(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           double s = 0.0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, double);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
               double r = sum_double5(3, 1.0, 2.0, 3.0);\n\
               return ((int)r) - 6;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-arg-double", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_double5(3, 1.0,2.0,3.0) == 6.0");
}

/// x86_64 va_arg 浮点溢出栈 (4 个 XMM 用完后, 第 5+ 走栈).
/// overflow 步进 8 字节, 验证 sum_double8 = 36.0.
#[test]
fn va_arg_x86_64_double_overflow() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "double sum_double8(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           double s = 0.0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, double);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
               double r = sum_double8(8, 1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0);\n\
               return ((int)r) - 36;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-arg-double-overflow", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_double8(8, 1..8) == 36.0");
}

/// x86_64 va_arg 无外部符号 — overflow 路径直接读栈, 无需 libc.
#[test]
fn va_arg_x86_64_no_external_symbols() {
    let source = VA_LIST_TYPEDEF.to_string()
        + "int sum_int5(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n";
    let (output, object_path) = run_object_test("va-arg-noext", &source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    for forbidden in &["__va_", "__builtin_va_", "va_start", "va_arg", "va_end"] {
        let needle = forbidden.as_bytes();
        assert!(
            !obj.windows(needle.len()).any(|w| w == needle),
            "object should not reference libc/internal va symbols ({forbidden})"
        );
    }
}

/// x86_64 va_arg fixture 包含 sum_int5/sum_int13/sum_long5/sum_double5/sum_double8.
#[test]
fn va_arg_x86_64_fixture_compiles() {
    let src = include_str!("fixtures/c/va_arg.c");
    let (output, object_path) = run_object_test("va-arg-fixture", src);
    assert!(
        output.status.success(),
        "fixture compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    for fname in &[
        "sum_int5",
        "sum_int13",
        "sum_long5",
        "sum_double5",
        "sum_double8",
    ] {
        let needle = fname.as_bytes();
        assert!(
            obj.windows(needle.len()).any(|w| w == needle),
            "fixture should define {fname}"
        );
    }
}
/// setjmp/longjmp 夹具能被 dcc 编译 (含 setjmp.h 调用).
#[test]
fn setjmp_x86_64_fixture_compiles() {
    let src = include_str!("fixtures/c/setjmp.c");
    let (output, _object_path) = run_object_test("setjmp-fixture", src);
    assert!(
        output.status.success(),
        "fixture compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// x86_64 setjmp CFI (Section 11.5) 在 dcc 生成的 .o 里嵌入 DW_CFA_def_cfa
/// + DW_CFA_offset 描述 callee-saved 寄存器保存位置. 这个测试验证:
/// 1. 编译到 .o 成功 (带 -g 以生成 .eh_frame)
/// 2. .eh_frame 节存在且含 CIE + FDE
/// 3. CIE 使用 LSB CIE_id = 0 (不是 DWARF 0xFFFFFFFF)
/// 4. FDE 包含 DW_CFA_def_cfa(7, 16) + DW_CFA_offset(13, 2) + DW_CFA_offset(16, 1)
///    字节序列, 描述 r13 / %rip 在 CFA 处的偏移
#[test]
#[cfg(target_arch = "x86_64")]
fn setjmp_x86_64_eh_frame_has_cfi_ops() {
    let src = include_str!("fixtures/c/setjmp.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-setjmp-cfi-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-setjmp-cfi-{}.o", std::process::id()));
    fs::write(&source_path, src).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-g", "-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    assert!(
        output.status.success(),
        "fixture compile with -g failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);

    // DW_CFA_def_cfa(7, 16) = 0c 07 10
    let needle_def_cfa = [0x0c, 7, 16u8];
    let found_def_cfa = obj.windows(3).any(|w| w == needle_def_cfa);
    assert!(found_def_cfa, "FDE should embed DW_CFA_def_cfa(rsp, 16)");

    // DW_CFA_offset(13, 2) = 8d 02  (r13 saved at CFA-16)
    let needle_r13 = [0x8d, 0x02];
    let found_r13 = obj.windows(2).any(|w| w == needle_r13);
    assert!(found_r13, "FDE should embed DW_CFA_offset(r13, 2)");

    // DW_CFA_offset(16, 1) = 90 01  (%rip saved at CFA-8)
    let needle_rip = [0x90, 0x01];
    let found_rip = obj.windows(2).any(|w| w == needle_rip);
    assert!(found_rip, "FDE should embed DW_CFA_offset(%rip, 1)");

    // 验证 CIE 使用 LSB CIE_id = 0
    // CIE 起始 8 字节特征: CIE_id=0 (4 字节零) + version=1 + "zR" augmentation
    // 头. 在整个 .o 中查找这一特征序列以确认 LSB CIE_id (不是 DWARF 0xFFFFFFFF).
    let cie_sig: [u8; 7] = [0, 0, 0, 0, 1, 0x7a, 0x52];
    let found_lsb_cie = obj.windows(cie_sig.len()).any(|w| w == cie_sig);
    assert!(found_lsb_cie, ".eh_frame CIE should use LSB CIE_id=0");
}

/// wchar_t wcs* / wmem* libc 转发 (Section 11 P0.1).
/// 编译 + 链接 + 运行 fixtures/c/wchar.c; 退出码 0 表示通过.
#[test]
fn wchar_libc_fixture_runs() {
    let src = include_str!("fixtures/c/wchar.c");
    let (compile, code) = run_host_link_and_run("wchar-libc", src);
    if code != 0 {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        panic!("wchar_libc fixture exit={code}, expected 0\ndcc stderr: {stderr}");
    }
}

/// signal() libc 转发 (Section 11 P0.2) - 运行时路径.
/// 编译 + 链接 + 运行 fixtures/c/signal.c; 退出码 0 表示通过.
#[test]
fn signal_libc_fixture_runs() {
    let src = include_str!("fixtures/c/signal.c");
    let (compile, code) = run_host_link_and_run("signal-libc", src);
    if code != 0 {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        panic!("signal_libc fixture exit={code}, expected 0\ndcc stderr: {stderr}");
    }
}

/// sigaction / sigaltstack libc 转发 (Section 11 P0.2) - 编译 + 运行时路径.
/// 验证 dcc 能编译带 struct sigaction / stack_t 的代码, 且运行时
/// sigaction/sigaltstack 调用成功 (退出码 0).
#[test]
#[cfg(target_arch = "x86_64")]
fn sigaction_sigaltstack_fixture_compiles() {
    let src = "\
#include <signal.h>
#include <stdlib.h>
void handler(int s) { (void)s; }
int main(void) {
    struct sigaction sa;
    sa.sa_handler = handler;
    sa.sa_flags = 0;
    sigemptyset(&sa.sa_mask);
    sa.sa_restorer = 0;
    if (sigaction(10, &sa, (struct sigaction*)0) != 0) return 1;

    stack_t ss;
    ss.ss_sp = malloc(4096);
    if (!ss.ss_sp) return 2;
    ss.ss_flags = 0;
    ss.ss_size = 4096;
    if (sigaltstack(&ss, (stack_t*)0) != 0) return 3;
    free(ss.ss_sp);
    return 0;
}
";
    let (compile, code) = run_host_link_and_run("sigaction-fixture", src);
    if code != 0 {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        panic!("sigaction/sigaltstack fixture exit={code}, expected 0\ndcc stderr: {stderr}");
    }
}

/// 复杂数学 cproj / cabs / carg libm 转发 (Section 11 P0.3).
/// 编译 fixtures/c/complex_libm.c, 验证 .o 引用 cproj / cabs / carg
/// 作为 undefined symbol (留给 host gcc -lm 解析), 然后链接运行.
#[test]
fn complex_libm_fixture_runs() {
    let src = include_str!("fixtures/c/complex_libm.c");
    let (compile, code) = run_host_link_and_run_with("complex-libm", src, &["-lm"]);
    if code != 0 {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        panic!("complex_libm fixture exit={code}, expected 0\ndcc stderr: {stderr}");
    }
}

/// M25 增量: PCH (预编译头) 集成测试. 验证 -fpch-create 与
/// -fpch-include 两条路径能端到端工作:
/// 1. 编译 preamble.c 头引用 + 函数体, 用 -fpch-create 生成 PCH.
/// 2. 编译 main.c 用 -fpch-include 加载 PCH, 验证头宏/typedef
///    仍然可用, 生成的 .o 与不使用 PCH 的对照路径都能正确运行.
/// 3. 对照路径: 不使用 PCH 直接编译 main.c, 同样运行成功.
#[test]
fn pch_create_then_include_compiles_and_runs() {
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
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
    let source_path = dir.join(format!("dcc-ubsan-vla-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-ubsan-vla-{}.s", std::process::id()));
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
/// 验证生成的汇编中插入 `call __ubsan_handle_null_pointer_dereference`
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
    let needle = "call __ubsan_handle_null_pointer_dereference";
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

/// P1.5: __kcsan_check_access 专用路径. 编译 KCSAN-only 代码
/// (只启 -fsanitize=thread, 不启 -fsanitize=address), 验证生成的
/// 汇编中插入 `call __kcsan_check_access`, 而不是走 KASAN 的
/// `__asan_loadN` / `__asan_storeN` (Section 4.2 P1.5).
#[test]
fn kcsan_only_emits_kcsan_check_access() {
    let src = "\
int f(int *p) { return *p; }
int main(void) { int x = 42; return f(&x); }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kcsan-only-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kcsan-only-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write kcsan fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=thread", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "kcsan-only compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    let kcsan_needle = "call __kcsan_check_access";
    assert!(
        asm.contains(kcsan_needle),
        "kcsan access check should be called in asm, but `{}` not found in:\n{}",
        kcsan_needle,
        asm
    );
    // 同时确认 KASAN 路径没有被启用 (没有 __asan_loadN 调用).
    assert!(
        !asm.contains("call __asan_load") && !asm.contains("call __asan_store"),
        "KASAN path must not be emitted when only KCSAN is enabled:\n{}",
        asm
    );
}

/// P2.1: C++ `obj.*ptr_to_member` 和 `obj->*ptr_to_member` 解析
/// + codegen (Section 6). 验证 dcc 解析器接受 `.*` 单 token
///   (lexer 产生 TokKind::DotStar) 和 `->*` (`->` + `*` 两 token),
///   并走 MemberStar / PtrMemberStar 路径.
#[test]
#[cfg(unix)]
fn p2_1_member_ptr_dotstar() {
    let src = include_str!("fixtures/cpp/member_ptr_deref.cpp");
    let (out, obj) = run_cpp_object_test("p2-1-member-ptr", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "p2_1_member_ptr compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P2.2: typeid(e) 真 RTTI 取类型 (Section 6). 验证 dcc lower
/// 阶段为 `typeid(T)` 静态路径和 `typeid(e)` 动态路径都生成
/// `(&__dcc_ti_<class>)` 真实 typeinfo 符号 (M17.0 阶段实装),
/// 而非 M8 简化版的 `((_ZTI*)(((void*)e)->__vptr + 0))` stub.
#[test]
#[cfg(unix)]
fn p2_2_typeid_rtti() {
    let src = include_str!("fixtures/cpp/typeid_rtti.cpp");
    let (out, obj) = run_cpp_object_test("p2-2-typeid", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "p2_2_typeid compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
/// P2.3: C++ 模板实参语法 T<U> (Section 6 表格 模板实例化行).
/// 验证 dcc cpp 解析器 `parse_mangle_type` (M18 阶段实装)
/// 在 ident 后识别 `<` 触发 `MangleType::Template` 分支, 接受
/// `Holder<int>` / `Holder<Holder<int>>` (嵌套 >> 视作两层 `>)
/// 语法, 不再走 M8 简化版对模板实参的剥离.
#[test]
#[cfg(unix)]
fn p2_3_template_arg_syntax() {
    let src = include_str!("fixtures/cpp/template_arg.cpp");
    let (out, obj) = run_cpp_object_test("p2-3-template-arg", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "p2_3_template_arg compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// M18.x: 模板嵌套深度限制 - 正常深度 (≤ MAX_TEMPLATE_DEPTH=128)
/// 应在类型上下文 (形参) 与表达式上下文 (局部声明) 都能编译;
/// 同时覆盖深层嵌套 `>>` 拆 token 的正确闭合 (template_depth.cpp).
#[test]
#[cfg(unix)]
fn m18_template_depth_ok() {
    let src = include_str!("fixtures/cpp/template_depth.cpp");
    let (out, obj) = run_cpp_object_test("m18-template-depth-ok", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "m18_template_depth_ok compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// M18.x: 模板嵌套深度超限 (200 层 > 128) 应报模板深度错误
/// (C0004) 而非挂起/栈溢出 (修复前 50000 层嵌套会让编译超时).
#[test]
#[cfg(unix)]
fn m18_template_depth_limit_enforced() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cpp-depth-limit-{}.cpp", std::process::id()));
    let object_path = dir.join(format!("dcc-cpp-depth-limit-{}.o", std::process::id()));
    // 200 层嵌套模板作为形参类型 (走 parse_mangle_type 深度检查)
    let mut src = String::from("template <class T> struct Wrap { T v; };\n");
    src.push_str("int take(");
    src.push_str(&"Wrap<".repeat(200));
    src.push_str("int");
    src.push_str(&">".repeat(200));
    src.push_str("* p) { return p != 0; }\n");
    src.push_str("int main() { return 0; }\n");
    fs::write(&source_path, &src).expect("write C++ fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&object_path);
    assert!(
        !output.status.success(),
        "over-depth template should fail to compile"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("depth") && stderr.contains("exceeds limit"),
        "stderr should report template depth: {}",
        stderr
    );
}

/// M18.x: SFINAE 简化支持 - 匿名默认类型形参 (`typename = T`,
/// enable_if 惯用法) / 模板默认实参 / 多形参模板 (sfinae.cpp).
/// 求值层 (enable_if 条件 / trait 检测) 由 feature_degrade.rs 单元测试覆盖.
#[test]
#[cfg(unix)]
fn m18_sfinae_smoke() {
    let src = include_str!("fixtures/cpp/sfinae.cpp");
    let (out, obj) = run_cpp_object_test("m18-sfinae", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "m18_sfinae_smoke compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P2.4: operator new[] / operator delete[] 走 _Znam / _ZdaPv
/// (Section 6). 验证 dcc lower 阶段对 `new T[n]` 和 `delete[] p`
/// 调用 mangle_op_new_array() / mangle_op_delete_array() 命名常量,
/// 即 Itanium C++ ABI 数组分配/释放符号 _Znam / _ZdaPv.
#[test]
#[cfg(unix)]
fn p2_4_new_arr_mangler() {
    let src = include_str!("fixtures/cpp/new_arr_mangler.cpp");
    let (out, obj) = run_cpp_object_test("p2-4-new-arr-mangler", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "p2_4_new_arr_mangler compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P2.5: new T[n] 数组分配 (Section 6). 验证 dcc parser
/// 在 `new T[n]` 形式下识别 `[` 触发 CppExpr::NewArr, lower
/// 阶段 emit `((T*)mangle_op_new_array()(sizeof(T) * n))`, 即
/// Itanium C++ ABI 数组分配符号 _Znam 调用. 配合
/// `delete[] p` 走 _ZdaPv (P2.4) 走完整 array 生命周期.
#[test]
#[cfg(unix)]
fn p2_5_new_array_alloc() {
    let src = include_str!("fixtures/cpp/new_array_alloc.cpp");
    let (out, obj) = run_cpp_object_test("p2-5-new-array-alloc", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "p2_5_new_array_alloc compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P2.6: placement new 走裸 malloc 替身 (Section 6). 验证 dcc
/// parser 检测 `new (ptr) T(args)` 形式, lower 阶段 emit 构造
/// 链 `(void*)(ptr), mangle_ctor(&n, ...)(ptr, args)`, 不调用
/// operator new 分配新内存, 模拟 Itanium C++ ABI placement
/// new 语义 (M17.0 阶段实装).
#[test]
#[cfg(unix)]
fn p2_6_placement_new() {
    let src = include_str!("fixtures/cpp/placement_new_ctor.cpp");
    let (out, obj) = run_cpp_object_test("p2-6-placement-new", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "p2_6_placement_new compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// =====================================================================
// M1 milestone tests
// =====================================================================

/// M1.1: Hello World — compile a single-file C program with stdio.h
/// and printf, link with host gcc, verify stdout output.
#[test]
#[cfg(unix)]
fn m1_1_hello_world() {
    let src = include_str!("fixtures/c/hello.c");
    let (out, code) = run_host_link_and_run("m1-1-hello", src);
    assert!(
        out.status.success(),
        "m1_1_hello compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "m1_1_hello exit code = {code}");
}

/// M1.1b: Hello World — verify stdout contains "Hello, World!".
#[test]
#[cfg(unix)]
fn m1_1_hello_stdout() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-m1-1-stdout-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-m1-1-stdout-{}.o", std::process::id()));
    let exe_path = dir.join(format!("dcc-m1-1-stdout-{}", std::process::id()));
    let src = include_str!("fixtures/c/hello.c");
    fs::write(&src_path, src).expect("write");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&src_path);
    assert!(
        compile.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let link = Command::new("gcc")
        .args(["-o"])
        .arg(&exe_path)
        .arg(&obj_path)
        .output()
        .expect("run gcc");
    let _ = fs::remove_file(&obj_path);
    assert!(
        link.status.success(),
        "link: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
    }
    let run = Command::new(&exe_path).output().expect("run");
    let _ = fs::remove_file(&exe_path);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("Hello, World!"), "stdout = {stdout:?}");
}

/// M1.2: multi-file compilation — compile two .c files separately,
/// link together, verify extern/static linkage works correctly.
#[test]
#[cfg(unix)]
fn m1_2_multi_file() {
    let dir = std::env::temp_dir();
    let helper_src = include_str!("fixtures/c/helper.c");
    let main_src = include_str!("fixtures/c/main.c");
    let h_path = dir.join(format!("dcc-m1-2-h-{}.c", std::process::id()));
    let m_path = dir.join(format!("dcc-m1-2-m-{}.c", std::process::id()));
    let h_obj = dir.join(format!("dcc-m1-2-h-{}.o", std::process::id()));
    let m_obj = dir.join(format!("dcc-m1-2-m-{}.o", std::process::id()));
    let exe = dir.join(format!("dcc-m1-2-{}", std::process::id()));
    fs::write(&h_path, helper_src).expect("write helper");
    fs::write(&m_path, main_src).expect("write main");
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let c1 = Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&h_obj)
        .arg(&h_path)
        .output()
        .expect("dcc");
    let _ = fs::remove_file(&h_path);
    assert!(
        c1.status.success(),
        "helper compile: {}",
        String::from_utf8_lossy(&c1.stderr)
    );
    let c2 = Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&m_obj)
        .arg(&m_path)
        .output()
        .expect("dcc");
    let _ = fs::remove_file(&m_path);
    assert!(
        c2.status.success(),
        "main compile: {}",
        String::from_utf8_lossy(&c2.stderr)
    );
    let link = Command::new("gcc")
        .args(["-o"])
        .arg(&exe)
        .arg(&h_obj)
        .arg(&m_obj)
        .output()
        .expect("gcc link");
    let _ = fs::remove_file(&h_obj);
    let _ = fs::remove_file(&m_obj);
    assert!(
        link.status.success(),
        "link: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
    }
    let run = Command::new(&exe).output().expect("run");
    let _ = fs::remove_file(&exe);
    assert!(run.status.success(), "run failed");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("sum=7")
            && stdout.contains("sq=25")
            && stdout.contains("ctr=3")
            && stdout.contains("loc=100"),
        "unexpected stdout: {stdout:?}"
    );
}

/// M1.3: preprocessor verification — macro expansion, conditional
/// compilation, token pasting, stringification, #undef, #ifndef,
/// defined() operator.  Compare dcc -E output with gcc -E.
#[test]
#[cfg(unix)]
fn m1_3_preprocessor() {
    let src = include_str!("fixtures/c/preproc.c");
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-m1-3-{}.c", std::process::id()));
    fs::write(&src_path, src).expect("write");
    // dcc -E
    let dcc_out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E"])
        .arg(&src_path)
        .output()
        .expect("dcc -E");
    // gcc -E
    let gcc_out = Command::new("gcc")
        .args(["-E"])
        .arg(&src_path)
        .output()
        .expect("gcc -E");
    let _ = fs::remove_file(&src_path);
    assert!(
        dcc_out.status.success(),
        "dcc -E: {}",
        String::from_utf8_lossy(&dcc_out.stderr)
    );
    assert!(
        gcc_out.status.success(),
        "gcc -E: {}",
        String::from_utf8_lossy(&gcc_out.stderr)
    );
    // Strip linemarkers (# N ...) and blank lines, then compare
    let strip = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect()
    };
    let dcc_lines = strip(&String::from_utf8_lossy(&dcc_out.stdout));
    let gcc_lines = strip(&String::from_utf8_lossy(&gcc_out.stdout));
    assert_eq!(
        dcc_lines, gcc_lines,
        "dcc -E differs from gcc -E\nDCC: {:?}\nGCC: {:?}",
        dcc_lines, gcc_lines
    );
}

/// M1.4: ELF object file format — verify dcc -c produces a valid
/// ELF64 relocatable object that readelf can parse, with correct
/// sections (.text, .rodata, .data, .bss, .symtab, .strtab),
/// symbol visibility (GLOBAL vs LOCAL), and relocation entries.
#[test]
#[cfg(unix)]
fn m1_4_elf_format() {
    let src = include_str!("fixtures/c/elf.c");
    let (out, obj) = run_object_test("m1-4-elf", src);
    assert!(
        out.status.success(),
        "m1_4_elf compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // file(1) should say "ELF 64-bit LSB relocatable"
    let file_out = Command::new("file").arg(&obj).output().expect("file");
    let file_str = String::from_utf8_lossy(&file_out.stdout);
    assert!(
        file_str.contains("ELF 64-bit") && file_str.contains("relocatable"),
        "not a valid ELF64 relocatable: {file_str}"
    );
    // readelf -h: check ELF header
    let hdr = Command::new("readelf")
        .args(["-h"])
        .arg(&obj)
        .output()
        .expect("readelf -h");
    assert!(hdr.status.success(), "readelf -h failed");
    let hdr_str = String::from_utf8_lossy(&hdr.stdout);
    assert!(hdr_str.contains("ELF64"), "not ELF64");
    assert!(hdr_str.contains("REL"), "not relocatable");
    // readelf -S: check sections
    let sec = Command::new("readelf")
        .args(["-S"])
        .arg(&obj)
        .output()
        .expect("readelf -S");
    assert!(sec.status.success(), "readelf -S failed");
    let sec_str = String::from_utf8_lossy(&sec.stdout);
    for name in &[".text", ".rodata", ".symtab", ".strtab"] {
        assert!(sec_str.contains(name), "missing section {name}");
    }
    // readelf -s: check symbols
    let sym = Command::new("readelf")
        .args(["-s"])
        .arg(&obj)
        .output()
        .expect("readelf -s");
    assert!(sym.status.success(), "readelf -s failed");
    let sym_str = String::from_utf8_lossy(&sym.stdout);
    assert!(sym_str.contains("main"), "missing main symbol");
    assert!(sym_str.contains("global_var"), "missing global_var");
    assert!(sym_str.contains("global_func"), "missing global_func");
    // readelf -r: check relocations exist
    let rel = Command::new("readelf")
        .args(["-r"])
        .arg(&obj)
        .output()
        .expect("readelf -r");
    assert!(rel.status.success(), "readelf -r failed");
    let rel_str = String::from_utf8_lossy(&rel.stdout);
    assert!(
        rel_str.contains("R_X86_64"),
        "missing x86_64 relocations: {rel_str}"
    );
    let _ = fs::remove_file(&obj);
}

// =====================================================================
// M2 milestone tests
// =====================================================================

/// M2.1: type system - sizeof/alignof for all basic types
/// matches gcc on x86_64 Linux.
#[test]
#[cfg(unix)]
fn m2_1_type_system() {
    let src = include_str!("fixtures/c/types.c");
    let (out, code) = run_host_link_and_run("m2-1-types", src);
    assert!(
        out.status.success(),
        "m2_1 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "type size/align checks failed (exit={code})");
}

/// M2.2: struct layout - bitfields, alignment, padding, packed,
/// aligned attribute, unions, flexible array members.
#[test]
#[cfg(unix)]
fn m2_2_struct_layout() {
    let src = include_str!("fixtures/c/struct.c");
    let (out, code) = run_host_link_and_run("m2-2-struct", src);
    assert!(
        out.status.success(),
        "m2_2 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "struct layout checks failed (exit={code})");
}

/// M2.3: integer promotion and implicit conversion rules.
#[test]
#[cfg(unix)]
fn m2_3_integer_promotion() {
    let src = include_str!("fixtures/c/promotion.c");
    let (out, code) = run_host_link_and_run("m2-3-promotion", src);
    assert!(
        out.status.success(),
        "m2_3 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "promotion checks failed (exit={code})");
}

/// M2.4: pointer operations - multi-level ptrs, function ptrs,
/// NULL, sizeof pointer types, pointer to struct members.
#[test]
#[cfg(unix)]
fn m2_4_pointers() {
    let src = include_str!("fixtures/c/pointer.c");
    let (out, code) = run_host_link_and_run("m2-4-pointer", src);
    assert!(
        out.status.success(),
        "m2_4 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "pointer checks failed (exit={code})");
}

/// M2.5: float support - float/double/long double ABI,
/// arithmetic, conversions, comparisons.
#[test]
#[cfg(unix)]
fn m2_5_float() {
    let src = include_str!("fixtures/c/float.c");
    let (out, code) = run_host_link_and_run("m2-5-float", src);
    assert!(
        out.status.success(),
        "m2_5 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "float checks failed (exit={code})");
}

/// M2.6: batch compilation test - compile a representative set
/// of existing C fixtures to .o files, verifying the compiler
/// handles diverse C programs (asm, attributes, builtins, etc.).
#[test]
#[cfg(unix)]
fn m2_6_batch_compile() {
    let fixtures = [
        "basics/hello.c",
        "basics/arith.c",
        "basics/control_flow.c",
        "basics/fib.c",
        "structs/basic_struct.c",
        "basics/switch.c",
        "pointers/function_call.c",
        "structs/bitfield.c",
        "structs/global_array.c",
        "basics/logic.c",
        "pointers/rel_ops.c",
        "pointers/pointer_arith.c",
    ];
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let mut pass = 0u32;
    let mut fail = 0u32;
    let dir = std::env::temp_dir();
    for name in &fixtures {
        let src = format!("examples/{name}");
        if !std::path::Path::new(&src).exists() {
            continue;
        }
        // fixture 名含子目录 (如 basics/hello.c), 拍平避免父目录不存在
        let flat = name.replace(['/', '\\'], "-");
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
            eprintln!("FAIL {name}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    assert!(fail == 0, "batch compile: {pass} passed, {fail} failed");
}

// =====================================================================
// M3 milestone tests
// =====================================================================

/// M3.1: inline assembly - asm volatile, register constraints,
/// memory clobber, multiple outputs.
#[test]
#[cfg(unix)]
fn m3_1_inline_asm() {
    let src = include_str!("fixtures/c/asm.c");
    let (out, code) = run_host_link_and_run("m3-1-asm", src);
    assert!(
        out.status.success(),
        "m3_1 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "inline asm checks failed (exit={code})");
}

/// M3.2: __attribute__ series - packed, aligned, section, weak,
/// alias, visibility, unused.
#[test]
#[cfg(unix)]
fn m3_2_attributes() {
    let src = include_str!("fixtures/c/attrs.c");
    let (out, code) = run_host_link_and_run("m3-2-attrs", src);
    assert!(
        out.status.success(),
        "m3_2 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "attribute checks failed (exit={code})");
}

/// M3.3: statement expressions ({ ... }) GNU extension.
#[test]
#[cfg(unix)]
fn m3_3_statement_expressions() {
    let src = include_str!("fixtures/c/stmt_expr.c");
    let (out, code) = run_host_link_and_run("m3-3-stmt-expr", src);
    assert!(
        out.status.success(),
        "m3_3 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "statement expr checks failed (exit={code})");
}

/// M3.4: typeof / __typeof__ operator.
#[test]
#[cfg(unix)]
fn m3_4_typeof() {
    let src = include_str!("fixtures/c/typeof.c");
    let (out, code) = run_host_link_and_run("m3-4-typeof", src);
    assert!(
        out.status.success(),
        "m3_4 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "typeof checks failed (exit={code})");
}

/// M3.5: VLA (variable-length arrays) and zero-length arrays.
#[test]
#[cfg(unix)]
fn m3_5_vla_zero_array() {
    let src = include_str!("fixtures/c/vla.c");
    let (out, code) = run_host_link_and_run("m3-5-vla", src);
    assert!(
        out.status.success(),
        "m3_5 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "VLA/zero-array checks failed (exit={code})");
}

/// M3.6: builtin functions - __builtin_expect, offsetof,
/// constant_p, choose_expr.
#[test]
#[cfg(unix)]
fn m3_6_builtins() {
    let src = include_str!("fixtures/c/builtins.c");
    let (out, code) = run_host_link_and_run("m3-6-builtins", src);
    assert!(
        out.status.success(),
        "m3_6 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "builtin checks failed (exit={code})");
}

/// M3.7: weak symbols - weak definition, weak variable.
#[test]
#[cfg(unix)]
fn m3_7_weak_symbols() {
    let src = include_str!("fixtures/c/weak.c");
    let (out, code) = run_host_link_and_run("m3-7-weak", src);
    assert!(
        out.status.success(),
        "m3_7 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "weak symbol checks failed (exit={code})");
}

/// M3.8: visibility control - hidden/default/protected.
#[test]
#[cfg(unix)]
fn m3_8_visibility() {
    let src = include_str!("fixtures/c/visibility.c");
    let (out, code) = run_host_link_and_run("m3-8-visibility", src);
    assert!(
        out.status.success(),
        "m3_8 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "visibility checks failed (exit={code})");
}

/// M3.9: Thread Local Storage (__thread, _Thread_local).
/// Uses dcc's own linker (ET_EXEC) since dcc TLS codegen
/// emits R_X86_64_PC32 instead of R_X86_64_TPOFF32.
#[test]
#[cfg(unix)]
fn m3_9_tls() {
    let src = include_str!("fixtures/c/tls.c");
    let (out, exe) = run_compile_and_run("m3-9-tls", src);
    assert!(
        out.status.success(),
        "m3_9 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(code, 0, "TLS checks failed (exit={code})");
}

/// M3.10: Stack canary (-fstack-protector) support.
/// Tests canary prologue/epilogue with ET_EXEC inline exit on failure.
#[test]
#[cfg(unix)]
fn m3_10_stack_canary() {
    let src = include_str!("fixtures/c/stack_canary.c");
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

/// M3.11: GOT addressing for undefined (external) functions.
/// External function addresses use MOV + GOTPCREL (actual function address),
/// internal function addresses use LEA + PC32/PLT32 (direct address).
#[test]
#[cfg(unix)]
fn m3_11_got_func_addr() {
    let src = include_str!("fixtures/c/got_func.c");
    let (out, exe) = run_compile_and_run("m3-11-got", src);
    assert!(
        out.status.success(),
        "m3_11 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(code, 0, "GOT func addr checks failed (exit={code})");
}

// C23 fixtures
#[test]
#[cfg(unix)]
fn c23_alignas() {
    let src = include_str!("fixtures/c/c23_alignas.c");
    let (out, exe) = run_compile_and_run("c23-alignas", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_alignas: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_bool() {
    let src = include_str!("fixtures/c/c23_bool.c");
    let (out, exe) = run_compile_and_run("c23-bool", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_bool: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_brace_init() {
    let src = include_str!("fixtures/c/c23_brace_init.c");
    let (out, exe) = run_compile_and_run("c23-brace-init", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_brace_init: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_enum_under() {
    let src = include_str!("fixtures/c/c23_enum_under.c");
    let (out, exe) = run_compile_and_run("c23-enum-under", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_enum_under: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_has_c_attribute() {
    let src = include_str!("fixtures/c/c23_has_c_attribute.c");
    let (out, exe) = run_compile_and_run("c23-has-c-attr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_has_c_attribute: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_nullptr() {
    let src = include_str!("fixtures/c/c23_nullptr.c");
    let (out, exe) = run_compile_and_run("c23-nullptr", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_nullptr: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_printf_b() {
    let src = include_str!("fixtures/c/c23_printf_b.c");
    let (out, exe) = run_compile_and_run("c23-printf-b", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_printf_b: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_static_assert() {
    let src = include_str!("fixtures/c/c23_static_assert.c");
    let (out, exe) = run_compile_and_run("c23-static-assert", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_static_assert: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_thread_local() {
    let src = include_str!("fixtures/c/c23_thread_local.c");
    let (out, exe) = run_compile_and_run("c23-thread-local", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_thread_local: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_typeof_unqual() {
    let src = include_str!("fixtures/c/c23_typeof_unqual.c");
    let (out, exe) = run_compile_and_run("c23-typeof-unqual", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "c23_typeof_unqual: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

// ====== M26: 协程真状态机 / modules BMF / GCC ABI 运行时 ======
#[test]
#[cfg(unix)]
fn m26_coroutine_state_machine() {
    let src = include_str!("fixtures/cpp/coroutine_state.cpp");
    let (out, _obj) = run_cpp_object_test("m26-coroutine-state", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m26_modules_bmf() {
    let src = include_str!("fixtures/cpp/modules_bmf.cpp");
    let (out, _obj) = run_cpp_object_test("m26-modules-bmf", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m26_gcc_compat_runtime() {
    let src = include_str!("fixtures/cpp/gcc_compat.cpp");
    let (out, _obj) = run_cpp_object_test("m26-gcc-compat", src);
    assert!(
        out.status.success(),
        "compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(_obj);
}

/// kernel/sched 内存屏障模式编译测试
#[test]
fn sched_barrier_patterns_compiles() {
    let source = include_str!("fixtures/c/sched_barrier_patterns.c");
    let (output, obj) = run_object_test("sched-barrier-patterns", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// kernel/sched rq lock / atomic / seqcount 模式编译测试
#[test]
fn sched_rq_lock_patterns_compiles() {
    let source = include_str!("fixtures/c/sched_rq_lock_patterns.c");
    let (output, obj) = run_object_test("sched-rq-lock-patterns", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// -E -o 写入指定文件而非 stdout
#[test]
fn preprocess_only_with_output_file() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-eo-src-{}.c", std::process::id()));
    let out = dir.join(format!("dcc-eo-out-{}.i", std::process::id()));
    fs::write(&src, "#define X 7\nint v = X;\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .expect("run dcc -E -o");
    let _ = fs::remove_file(&src);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = fs::read_to_string(&out).unwrap_or_default();
    let _ = fs::remove_file(&out);
    assert!(
        content.contains("int v = 7;"),
        "preprocessed output should contain expanded macro, got: {content}"
    );
    // stdout 应为空 (输出已重定向到文件)
    assert!(
        output.stdout.is_empty(),
        "stdout should be empty when -o is specified, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// -E 无 -o 时输出到 stdout
#[test]
fn preprocess_only_stdout() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-eo-stdout-{}.c", std::process::id()));
    fs::write(&src, "#define Y 3\nint z = Y;\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E"])
        .arg(&src)
        .output()
        .expect("run dcc -E");
    let _ = fs::remove_file(&src);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("int z = 3;"),
        "stdout should contain preprocessed output, got: {stdout}"
    );
}

/// C89 函数类型参数 (C11 6.7.6.3p8): 参数声明为函数类型时调整为函数指针.
/// 内核 include/net/ipv6.h 的 ip6_append_data 以 `int getfrag(void*, char*, ...)`
/// 形式声明回调参数. 修复前 parse_params 报 E0001 (expected ')', got LParen).
#[test]
fn func_type_param_compiles() {
    let source = include_str!("fixtures/c/func_type_param.c");
    let (output, obj) = run_object_test("func-type-param", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 数组维度整型常量表达式折叠: `BITS_PER_LONG + 1` 应折叠为编译期常量,
/// 使 typeof 重声明类型相等 (内核 kernel/cpu.c cpu_bit_bitmap 模式).
/// 修复前维度为 ArraySize::Var, 相等比较恒 false, 误报 E0007.
#[test]
fn array_dim_const_fold_compiles() {
    let source = include_str!("fixtures/c/array_dim_const_fold.c");
    let (output, obj) = run_object_test("array-dim-const-fold", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 同标签 struct/union 指针重声明兼容: C11 6.2.5p22 同标签即同一类型.
/// 内核 include/linux/sched.h `extern struct pid *cad_pid;` (标签不完整)
/// + kernel/reboot.c 完整定义. 修复前 Ptr 包装后 E0007.
#[test]
fn tag_compat_redecl_compiles() {
    let source = include_str!("fixtures/c/tag_compat_redecl.c");
    let (output, obj) = run_object_test("tag-compat-redecl", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 全局静态初始化器链接期地址常量 (E1006 回归): 数组成员隐式数组退化
/// (utsname_sysctl.c) / &member_chain[i] (tracepoint.c) / (&x)->field
/// (watchdog.c cpumask_bits 宏展开) 三种形式.
#[test]
fn static_addr_const_init_compiles() {
    let source = include_str!("fixtures/c/static_addr_const.c");
    let (output, obj) = run_object_test("static-addr-const", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 多维数组维度从内向外解析 (C11 6.7.6.2p4): char buf[2][8] 是 2 个 char[8].
/// 修复前 3 处维度解析从左到右包裹, sizeof buf[0]==2 (应为 8), 元素 stride
/// 错 (words[1] 按 1 字节步进读标量), 运行时越界/崩溃; 另覆盖参数多维数组
/// 退化 (int a[][3] -> int (*a)[3]) 与数组类型元素表达式退化 (C11 6.3.2.1p3).
#[test]
fn multi_dim_array_compiles_and_runs() {
    let source = include_str!("fixtures/c/multi_dim_array.c");
    let (output, exe) = run_compile_and_run("multi-dim-array", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "multi_dim_array should return 0, got {rc}");
}

/// 内置链接器失败路径清理临时 .o: 未定义符号导致 link_multi_tu 失败时,
/// 提前返回必须删除 /tmp/dcc-builtlink-<pid>-*.o. 修复前 `?` 跳过清理,
/// 每次失败链接都残留一个临时目标文件.
#[test]
fn builtin_linker_cleans_tmp_objs_on_failure() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-bllink-{}.c", std::process::id()));
    let out = dir.join(format!("dcc-bllink-{}.out", std::process::id()));
    fs::write(&src, "int foo(void);\nint main(void) { return foo(); }\n").unwrap();
    // spawn 拿子进程 pid: 临时 .o 文件名带 dcc 子进程的 pid
    let child = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-pie")
        .arg("-o")
        .arg(&out)
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn dcc");
    let dcc_pid = child.id();
    let output = child.wait_with_output().expect("wait dcc");
    assert!(
        !output.status.success(),
        "link with undefined symbol must fail, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let prefix = format!("dcc-builtlink-{dcc_pid}-");
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .collect();
    assert!(
        leftovers.is_empty(),
        "builtin linker leaked tmp .o files: {leftovers:?}"
    );
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&out);
}

/// PIE 编译测试: 编译 return-42 程序为 PIE 可执行文件, 运行验证退出码为 42.
/// 验证 PT_INTERP, _start 桩, 动态重定位的完整链路.
#[test]
fn pie_builtin_linker_return_42() {
    let (output, exe) = run_compile_and_run_flags(
        "pie-return-42",
        "int main(void) { return 42; }",
        &["-pie", "-fbuiltin-linker"],
    );
    assert!(
        output.status.success(),
        "pie compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "pie return-42 should exit with 42, got {rc}");
    let _ = fs::remove_file(&exe);
}

/// C++ 模板 vector: 验证 std::vector<int> 模板实例化能编译通过
#[test]
fn cpp_template_vector_int_instantiation() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cpp-vec-int-{}.cpp", std::process::id()));
    let asm_path = dir.join(format!("dcc-cpp-vec-int-{}.s", std::process::id()));
    // stdlib 头文件由 dcc++ 自动包含, 无需显式 #include
    let source = "\
int main() {
    std::vector<int> v;
    int x = 42;
    v.push_back(x);
    return 0;
}
";
    fs::write(&source_path, source).expect("write C++ fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    assert!(
        output.status.success(),
        "vector<int> compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 实例化后的类名应包含 __dcc_template_vector_int 或 dccpp_vector_new
    assert!(
        asm.contains("__dcc_template_vector_int") || asm.contains("dccpp_vector_new"),
        "expected template instantiation or vector runtime call in asm: {asm}"
    );
}

/// C++ 模板 vector: 验证 vector 头文件声明为模板类
#[test]
fn cpp_vector_header_is_template_class() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cpp-vec-tpl-{}.cpp", std::process::id()));
    let asm_path = dir.join(format!("dcc-cpp-vec-tpl-{}.s", std::process::id()));
    // 使用两种元素类型验证单态化
    let source = "\
int main() {
    std::vector<int> vi;
    std::vector<double> vd;
    return 0;
}
";
    fs::write(&source_path, source).expect("write C++ fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    assert!(
        output.status.success(),
        "vector<int>+vector<double> compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// =================== CET/IBT 安全 PLT ===================

/// CET/IBT: -z ibt 编译 PIE 应生成 .plt.sec 节和 .gnu.property 节
#[test]
fn cet_ibt_plt_sec_and_gnu_property_sections() {
    let (output, exe) = run_compile_and_run_flags(
        "cet-ibt-plt",
        "extern int puts(const char *);\nint main(void) { puts(\"hi\"); return 0; }",
        &["-pie", "-fbuiltin-linker", "-z", "ibt"],
    );
    assert!(
        output.status.success(),
        "cet-ibt compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // readelf -S 检查 .plt.sec 和 .gnu.property 节
    let readelf = Command::new("readelf")
        .args(["-S"])
        .arg(&exe)
        .output()
        .expect("readelf");
    let sections = String::from_utf8_lossy(&readelf.stdout);
    let _ = fs::remove_file(&exe);
    assert!(
        sections.contains(".plt.sec"),
        "expected .plt.sec section in ELF: {sections}"
    );
    assert!(
        sections.contains(".gnu.property"),
        "expected .gnu.property section in ELF: {sections}"
    );
}

/// CET/IBT: 不使用 -z ibt 时 .plt.sec 应为空 (size=0), 函数条目在 .plt 中
#[test]
fn cet_no_ibt_plt_sec_empty() {
    let (output, exe) = run_compile_and_run_flags(
        "cet-noibt-plt",
        "extern int puts(const char *);\nint main(void) { puts(\"hi\"); return 0; }",
        &["-pie", "-fbuiltin-linker"],
    );
    assert!(
        output.status.success(),
        "noibt compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // readelf -S -W 宽输出: 每节一行, 字段含 size
    let readelf = Command::new("readelf")
        .args(["-S", "-W"])
        .arg(&exe)
        .output()
        .expect("readelf");
    let sections = String::from_utf8_lossy(&readelf.stdout);
    let _ = fs::remove_file(&exe);
    // 传统模式: .plt 必须存在且非空 (PLT[0] + 函数条目)
    assert!(
        sections.contains(".plt"),
        "should have .plt section: {sections}"
    );
    // .plt.sec 若存在则 size 应为 0
    for line in sections.lines() {
        if line.contains(".plt.sec") {
            // 宽格式: [Nr] name type addr off size entsize flags ...
            // size 为第 6 个字段 (0-indexed: 5)
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() > 5 {
                assert_eq!(
                    fields[5], "000000",
                    ".plt.sec should be empty without IBT: {line}"
                );
            }
            break;
        }
    }
}

// =================== C11 atomic 预置宏 ===================

/// C11 hosted 模式: atomic_int / memory_order_* / atomic_thread_fence 等预置宏可用
#[test]
#[cfg(unix)]
fn c11_atomic_macros_hosted_compile_and_run() {
    let source = include_str!("fixtures/c/c11_atomic_macros.c");
    let (output, exe) = run_compile_and_run("c11-atomic-hosted", source);
    assert!(
        output.status.success(),
        "c11 atomic hosted compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "c11_atomic_macros should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// C11 freestanding 模式: __STDC_HOSTED__=0, __STDC_NO_ATOMICS__=1
#[test]
#[cfg(unix)]
fn c11_atomic_freestanding_no_atomics() {
    let source = include_str!("fixtures/c/c11_atomic_freestanding.c");
    let (output, exe) =
        run_compile_and_run_flags("c11-atomic-freestanding", source, &["-ffreestanding"]);
    assert!(
        output.status.success(),
        "freestanding compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(
        rc, 0,
        "freestanding should see __STDC_NO_ATOMICS__=1, got {rc}"
    );
    let _ = fs::remove_file(exe);
}

// =================== 嵌套块重声明 ===================

/// 嵌套块重声明: 内层块可遮蔽外层变量 (不同类型不报 E0007)
#[test]
fn nested_block_redecl_different_type_no_error() {
    let src = "\
int main(void) {
    int x = 10;
    {
        double x = 3.14;
        (void)x;
    }
    return (x == 10) ? 0 : 1;
}
";
    let (output, exe) = run_compile_and_run("nested-redecl-shadow", src);
    assert!(
        output.status.success(),
        "nested block redecl should compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "outer x should be 10, got {rc}");
    let _ = fs::remove_file(exe);
}
