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
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-{name}-{}.c", std::process::id()));
    let exe_path = dir.join(format!("dcc-{name}-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
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
    let stub_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tools/libdecnumber_stub");
    let combined = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tools/libdecnumber_combined.c");
    let dir = std::env::temp_dir();
    let object = dir.join(format!("dcc-libdecnumber-{}.o", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-I", gcc_src.join("libdecnumber").to_str().unwrap(),
            "-I", gcc_src.join("libdecnumber/dpd").to_str().unwrap(),
            "-I", stub_dir.to_str().unwrap(),
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        asm_text.contains(".section .init.text"),
        "expected custom section directive: {asm_text}"
    );
    assert!(
        asm_text.contains("custom_function:"),
        "expected custom_function label: {asm_text}"
    );
    let init_pos = asm_text.find(".section .init.text").expect("init.text section");
    let custom_pos = asm_text.find("custom_function:").expect("custom_function label");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(!dep_text.contains("sys.h"), "MMD should skip system header: {dep_text}");
}

#[test]
fn flto_summary_includes_function_size() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-lto-size-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-lto-size-{}.o", std::process::id()));
    fs::write(&source_path, "int helper(void) { return 1; }\nint main(void) { return helper(); }\n")
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&obj_bytes);
    assert!(text.contains("helper:"), "LTO summary missing helper: {text}");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let obj_str = String::from_utf8_lossy(&obj);
    assert!(obj_str.contains(".debug_info"), "missing .debug_info in object");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        obj.windows(5).any(|w| w == [0xB8, 0x05, 0x00, 0x00, 0x00]),
        "expected mov $5, %eax after constant fold"
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(dep_text.contains(&hdr_name), "dep file missing header: {dep_text}");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let obj_str = String::from_utf8_lossy(&obj);
    assert!(obj_str.contains(".eh_frame"), "missing .eh_frame");
    assert!(obj_str.contains(".orc_unwind_ip"), "missing .orc_unwind_ip");
}

#[test]
fn global_asm_exports_symbol() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-gasm-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-gasm-{}.o", std::process::id()));
    let source = "asm(\".globl global_marker\\nglobal_marker:\\n\");\nint main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&out_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let obj = fs::read(&out_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(out_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
        assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    assert!(
        String::from_utf8_lossy(&merged).contains(".llvm.lto"),
        "linked object missing merged .llvm.lto"
    );
}

/// glibc string.h：`_Generic(0 ? ... : ..., const void *: ..., default: ...)`
/// 三元 else 不得吞掉 association 列表的逗号。
#[test]
fn generic_with_ternary_controlling_expr_compiles() {
    let source = include_str!("fixtures/generic_ternary.c");
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
    let source = include_str!("fixtures/generic_pointer.c");
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
    let source = include_str!("../examples/test_asm_constraints.c");
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
    let source = include_str!("../examples/test_p4_va_list.c");
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
            "--target", "aarch64-linux-gnu",
            "-c", "-O0",
            "-o", object_path.to_str().unwrap(),
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
            "--target", "aarch64-linux-gnu",
            "-c", "-O0",
            "-o", object_path.to_str().unwrap(),
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
    assert_eq!(par_bytes.len(), ser_bytes.len(), "parallel/serial .text size mismatch");
    assert_eq!(par_bytes, ser_bytes, "parallel and serial objects must match");
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
            "--target", "aarch64-linux-gnu",
            "-c", "-O0",
            "-o", object_path.to_str().unwrap(),
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
            "--target", "riscv64-linux-gnu",
            "-c", "-O0",
            "-o", object_path.to_str().unwrap(),
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
            "--target", "riscv64-linux-gnu",
            "-c", "-O0",
            "-o", object_path.to_str().unwrap(),
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
    let source = include_str!("fixtures/stmt_expr_call.c");
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
    let source = include_str!("fixtures/typeof_struct.c");
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
    let source = include_str!("fixtures/percpu_decl.c");
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
    let source = include_str!("fixtures/section_concat.c");
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
    let source = include_str!("fixtures/gdt_designated_init.c");
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
    let source = include_str!("fixtures/bitfield_comma.c");
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
    let source = include_str!("fixtures/asm_inline_qual.c");
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
    let source = include_str!("fixtures/struct_empty_field.c");
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
    let source = include_str!("fixtures/auto_type.c");
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
    let source = include_str!("fixtures/static_assert_offsetof.c");
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
    let source = include_str!("fixtures/div_round_up_macro.c");
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
    let source = include_str!("fixtures/multidim_array_field.c");
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
    let source = include_str!("fixtures/attr_ptr_func_decl.c");
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
    let source = include_str!("fixtures/vfs_pressure_stmt_expr.c");
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
    let source = include_str!("fixtures/gnu_elvis_op.c");
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
    let source = include_str!("fixtures/current_thread_info_macro.c");
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
    let source = include_str!("fixtures/ternary_ptr_deref.c");
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
    let source = include_str!("fixtures/asm_local_label.c");
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
    let source = include_str!("fixtures/asm_label_insn.c");
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
    let source = include_str!("fixtures/may_alias_struct.c");
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
    let source = include_str!("fixtures/nofail_stringify.c");
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
    let source = include_str!("fixtures/compat_typedef.c");
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
    let source = include_str!("fixtures/shadow_libc_name.c");
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
    let source = include_str!("fixtures/typeof_generic_read_once.c");
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
    let source = include_str!("fixtures/asm_goto_hash_comment.c");
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
    let source = include_str!("fixtures/vector_binop_non_ident.c");
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
    let source = include_str!("fixtures/vector_global.c");
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
    let source = include_str!("fixtures/vector_global_run.c");
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
    let source = include_str!("fixtures/vector_init_rvalue.c");
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
    let source = include_str!("fixtures/complex_div.c");
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
    let source = include_str!("fixtures/object_size_builtin.c");
    let (output, exe) = run_compile_and_run("object-size-builtin", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "object_size_builtin should return 0, got {rc}");
}

/// __builtin_object_size FORTIFY 模式（数组/成员）
#[test]
fn builtin_object_size_fortify_run_returns_zero() {
    let source = include_str!("fixtures/builtin_object_size_fortify.c");
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
    let source = include_str!("fixtures/builtin_apply_return.c");
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
    let source = include_str!("fixtures/builtin_prefetch.c");
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
    let source = include_str!("fixtures/asm_cmpxchg_mem.c");
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
    let source = include_str!("fixtures/asm_extended.c");
    let (output, _obj) = run_object_test("asm-extended", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// __int128 SysV 双寄存器传参与返回值
#[test]
fn int128_call_abi_run_returns_zero() {
    let source = include_str!("fixtures/int128_call_abi.c");
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
    let source = include_str!("fixtures/int128_mul_div_mod.c");
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
    let source = include_str!("fixtures/float_mod.c");
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
    let source = include_str!("fixtures/int128_logical.c");
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
    let source = include_str!("fixtures/asm_constraints_ext.c");
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
    let source = include_str!("fixtures/global_indexed.c");
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
    let source = include_str!("fixtures/vector_float_scalar.c");
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
    let source = include_str!("fixtures/asm_kernel_extra.c");
    let (output, _obj) = run_object_test("asm-kernel-extra", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// mini-asm 补全 (高): 段寄存器 / 系统指令 / 内核启动 / 虚拟化指令
#[test]
fn asm_paravirt_boot_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/asm_paravirt_boot.c");
    let (output, _obj) = run_object_test("asm-paravirt-boot", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// 完整指令子集 (M13): AVX/AVX2/AVX-VNNI + AES-NI/SHA + FMA/F16C + TSX + AMX
#[test]
fn asm_avx_aes_fma_tsx_compiles_without_nop_fallback() {
    let source = include_str!("fixtures/asm_avx_aes_fma_tsx.c");
    let (output, _obj) = run_object_test("asm-avx-aes-fma-tsx", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("emitted as nop"),
        "unexpected mini-asm nop fallback: {stderr}"
    );
}

/// __builtin_constant_p：编译期常量判定（含 __int128 表达式）
#[test]
fn builtin_constant_p_full_run_returns_zero() {
    let source = include_str!("fixtures/builtin_constant_p_full.c");
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
    let source = include_str!("fixtures/test_m12_builtins.c");
    let (output, exe) = run_compile_and_run("builtin-m12-extra", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "test_m12_builtins should return 0, got {rc}");
}

/// M12+ builtins: rotate / clrsb / fma 综合测试
#[test]
fn builtin_rotate_clrsb_run_returns_zero() {
    let source = include_str!("fixtures/builtin_rotate_clrsb.c");
    let (output, exe) = run_compile_and_run("builtin-rotate-clrsb", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "rotate/clrsb/fma should return 0, got {rc}");
}

/// M12+ builtins: strcasecmp / strncasecmp (走 libc)
#[test]
fn builtin_strcasecmp_run_returns_zero() {
    let source = include_str!("fixtures/builtin_strcasecmp.c");
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
    let source = include_str!("fixtures/builtin_file_line_func.c");
    let (output, exe) = run_compile_and_run("builtin-file-line-func", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "file/line/func should return 0, got {rc}");
}

/// M12+ builtins: classify_type / types_compatible_p
#[test]
fn builtin_classify_compat_run_returns_zero() {
    let source = include_str!("fixtures/builtin_classify_compat.c");
    let (output, exe) = run_compile_and_run("builtin-classify-compat", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "classify/compat should return 0, got {rc}");
}

/// M12+ builtins: choose_expr + constant_p + object_size 综合
#[test]
fn builtin_choose_full_run_returns_zero() {
    let source = include_str!("fixtures/builtin_choose.c");
    let (output, exe) = run_compile_and_run("builtin-choose-full", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "choose should return 0, got {rc}");
}

/// always_inline / noinline：IR 内联 pass 尊重属性
#[test]
fn inline_attrs_o1_run_returns_zero() {
    let source = include_str!("fixtures/inline_attrs.c");
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
    let source = include_str!("fixtures/unknown_builtin.c");
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
fn run_compile_o(name: &str, source: &str, opt: &str) -> (std::process::Output, std::path::PathBuf) {
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
    let source = include_str!("fixtures/always_inline_o0.c");
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
    let source = include_str!("fixtures/opt_gvn.c");
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
    let source = include_str!("fixtures/opt_loop_unroll.c");
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
    let source = include_str!("fixtures/opt_loop_vectorize.c");
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
    let source = include_str!("fixtures/static_key_maybe.c");
    let output = run_syntax_test("static-key-maybe", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn static_key_chain_compiles() {
    let source = include_str!("fixtures/static_key_chain.c");
    let output = run_syntax_test("static-key-chain", source);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn static_key_decl_compiles() {
    let source = include_str!("fixtures/static_key_decl.c");
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
        assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    }
    let link = Command::new(dcc)
        .args(["-r", "-flto", "-o"])
        .arg(&out_o)
        .arg(&a_o)
        .arg(&b_o)
        .output()
        .expect("link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
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
    assert!(symtab.contains("exported"), "exported must survive link: {symtab}");
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
    let source = include_str!("fixtures/ms_extensions.c");
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
    let source = include_str!("fixtures/attr_cleanup.c");
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
    let source = include_str!("fixtures/attr_target_clones.c");
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
    let source = include_str!("fixtures/attr_no_sanitize.c");
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

/// __attribute__((copy(fn))) / __attribute__((returns_nonnull))
#[test]
#[cfg(unix)]
fn attr_copy_returns_nonnull_compile_and_run() {
    let source = include_str!("fixtures/attr_copy_returns_nonnull.c");
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
    let source = include_str!("fixtures/pragma_family.c");
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
    let source = include_str!("fixtures/pragma_operator.c");
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
    let source = include_str!("fixtures/user_diagnostics.c");
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
    let source = include_str!("fixtures/static_assert_msg.c");
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
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    assert!(
        String::from_utf8_lossy(&merged).contains(".llvm.lto"),
        "single-object -r must preserve .llvm.lto"
    );
}

#[test]
#[cfg(unix)]
fn thread_local_exec_returns_15() {
    let src = include_str!("fixtures/thread_local.c");
    let (out, exe) = run_compile_and_run("tls", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 15, "tls counter");
    let _ = fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn kr_old_style_function() {
    let src = include_str!("fixtures/kr_func.c");
    let (out, exe) = run_compile_and_run("kr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 7);
    let _ = fs::remove_file(&exe);
}

#[test]
fn format_attr_warns_on_missing_arg() {
    let src = "__attribute__((format(printf, 1, 2))) void p(const char *f, ...);\n\
               int main(void) { p(\"%d %d\", 1); return 0; }\n";
    let output = run_syntax_test("fmt-warn", src);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("W0003"), "expected format warning: {stderr}");
}

#[test]
#[cfg(unix)]
fn stdatomic_macros_hosted() {
    let src = include_str!("fixtures/atomic_macros.c");
    let (out, exe) = run_compile_and_run("atomic", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(output.status.success(), "permissive should not error: {stderr}");
    assert!(stderr.contains("W0002"), "expected permissive warning: {stderr}");
}

#[test]
#[cfg(unix)]
fn exec_elf_has_symtab() {
    let src = "int g = 1; int main(void) { return g; }\n";
    let (out, exe) = run_compile_and_run("symtab", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    let src = include_str!("fixtures/m15_float_const.c");
    let (out, exe) = run_compile_and_run("m15-float-const", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 字符串字面量求值 + 拼接 (M15.1)
#[test]
#[cfg(unix)]
fn m15_string_literal_and_concat() {
    let src = include_str!("fixtures/m15_string_const.c");
    let (out, exe) = run_compile_and_run("m15-string-concat", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 地址常量求值 (M15.1)
#[test]
#[cfg(unix)]
fn m15_address_const() {
    let src = include_str!("fixtures/m15_address_const.c");
    let (out, exe) = run_compile_and_run("m15-addr-const", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// __builtin_* 编译期折叠 (strlen/abs/bswap)
#[test]
#[cfg(unix)]
fn m15_builtin_fold() {
    let src = include_str!("fixtures/m15_builtin_fold.c");
    let (out, exe) = run_compile_and_run("m15-builtin-fold", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 严格类型检查: 隐式指针/整数转换 warning (不阻塞)
#[test]
fn m15_strict_warning_compile() {
    let src = include_str!("fixtures/m15_strict_warning.c");
    let path = std::env::temp_dir().join(format!("dcc-m15-strict-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-Wint-conversion", "-Wint-truncation", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&path);
    assert!(output.status.success(), "strict-warn should not error: {}",
            String::from_utf8_lossy(&output.stderr));
}

/// -fbounds-check 数组越界检测 (正常路径不触发, 仅编译 + 运行)
#[test]
#[cfg(unix)]
fn m15_bounds_check_inrange() {
    let src = include_str!("fixtures/m15_bounds_check.c");
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-m15-bounds-{pid}.c"));
    let exe = dir.join(format!("dcc-m15-bounds-{pid}"));
    fs::write(&sp, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fbounds-check", "-o"]).arg(&exe).arg(&sp)
        .output().expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// C23 constexpr 函数声明 (解析通过, 运行结果正确)
#[test]
#[cfg(unix)]
fn m15_constexpr_function() {
    let src = include_str!("fixtures/m15_constexpr.c");
    let (out, exe) = run_compile_and_run("m15-constexpr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

// ===== M16: SSA IR + 优化 pass (DCE / SR / SCCP / TCO / ...) =====

/// SSA IR 基础 - if/else/while/for 控制流正确转换
#[test]
#[cfg(unix)]
fn m16_ssa_ir() {
    let src = include_str!("fixtures/m16_ssa_ir.c");
    let (out, exe) = run_compile_and_run("m16-ssa-ir", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 强度削减: i * 2^N -> i << N
#[test]
#[cfg(unix)]
fn m16_strength_reduction() {
    let src = include_str!("fixtures/m16_strength_reduction.c");
    let (out, exe) = run_compile_and_run("m16-strength", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 跨函数 DCE: 不可达 static 函数被删除
#[test]
#[cfg(unix)]
fn m16_cross_function_dce() {
    let src = include_str!("fixtures/m16_cross_function_dce.c");
    let (out, exe) = run_compile_and_run("m16-dce", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// SCCP: 编译期常量分支折叠
#[test]
#[cfg(unix)]
fn m16_sccp() {
    let src = include_str!("fixtures/m16_sccp.c");
    let (out, exe) = run_compile_and_run("m16-sccp", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 循环优化 (Tiling / IVOP / Interchange)
#[test]
#[cfg(unix)]
fn m16_loop_opt() {
    let src = include_str!("fixtures/m16_loop_opt.c");
    let (out, exe) = run_compile_and_run("m16-loop-opt", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// TCO: 尾递归求值
#[test]
#[cfg(unix)]
fn m16_tco() {
    let src = include_str!("fixtures/m16_tco.c");
    let (out, exe) = run_compile_and_run("m16-tco", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 函数属性传播 (cold / hot)
#[test]
#[cfg(unix)]
fn m16_function_attr() {
    let src = include_str!("fixtures/m16_function_attr.c");
    let (out, exe) = run_compile_and_run("m16-attr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

// ===== M17: C++ 修饰符与类型推导 (14 项) =====

/// M17.0: auto 真类型推导 (int / StringLit / class)
#[test]
#[cfg(unix)]
fn m17_auto_deduce() {
    let src = include_str!("fixtures/m17_auto_deduce.cpp");
    let (out, _obj) = run_cpp_object_test("m17-auto-deduce", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: decltype(expr) 真类型查询
#[test]
#[cfg(unix)]
fn m17_decltype() {
    let src = include_str!("fixtures/m17_decltype.cpp");
    let (out, _obj) = run_cpp_object_test("m17-decltype", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: decltype(auto) 变量类型推导
#[test]
#[cfg(unix)]
fn m17_decltype_auto() {
    let src = include_str!("fixtures/m17_decltype_auto.cpp");
    let (out, _obj) = run_cpp_object_test("m17-decltype-auto", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: explicit 构造函数
#[test]
#[cfg(unix)]
fn m17_explicit_ctor() {
    let src = include_str!("fixtures/m17_explicit_ctor.cpp");
    let (out, _obj) = run_cpp_object_test("m17-explicit-ctor", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: final / override 关键字
#[test]
#[cfg(unix)]
fn m17_final_override() {
    let src = include_str!("fixtures/m17_final_override.cpp");
    let (out, _obj) = run_cpp_object_test("m17-final-override", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: friend 关键字 (lower 接受 friend 声明, 不强求链接)
#[test]
#[cfg(unix)]
fn m17_friend() {
    let src = include_str!("fixtures/m17_friend.cpp");
    let (out, _obj) = run_cpp_object_test("m17-friend", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: if constexpr 编译期分支
#[test]
#[cfg(unix)]
fn m17_if_constexpr() {
    let src = include_str!("fixtures/m17_if_constexpr.cpp");
    let (out, _obj) = run_cpp_object_test("m17-if-constexpr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: using Base::Base; 继承构造
#[test]
#[cfg(unix)]
fn m17_inheriting_ctor() {
    let src = include_str!("fixtures/m17_inheriting_ctor.cpp");
    let (out, _obj) = run_cpp_object_test("m17-inheriting-ctor", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: 成员指针 obj.*ptr_to_member
#[test]
#[cfg(unix)]
fn m17_member_ptr() {
    let src = include_str!("fixtures/m17_member_ptr.cpp");
    let (out, _obj) = run_cpp_object_test("m17-member-ptr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: mutable 独立字段
#[test]
#[cfg(unix)]
fn m17_mutable() {
    let src = include_str!("fixtures/m17_mutable.cpp");
    let (out, _obj) = run_cpp_object_test("m17-mutable", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: new T[n] 数组分配 + operator new[]/delete[]
#[test]
#[cfg(unix)]
fn m17_new_array() {
    let src = include_str!("fixtures/m17_new_array.cpp");
    let (out, _obj) = run_cpp_object_test("m17-new-array", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: noexcept 关键字
#[test]
#[cfg(unix)]
fn m17_noexcept() {
    let src = include_str!("fixtures/m17_noexcept.cpp");
    let (out, _obj) = run_cpp_object_test("m17-noexcept", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: placement new
#[test]
#[cfg(unix)]
fn m17_placement_new() {
    let src = include_str!("fixtures/m17_placement_new.cpp");
    let (out, _obj) = run_cpp_object_test("m17-placement-new", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: smoke 测试 (最小可工作 C++ 程序)
#[test]
#[cfg(unix)]
fn m17_smoke() {
    let src = include_str!("fixtures/m17_smoke.cpp");
    let (out, _obj) = run_cpp_object_test("m17-smoke", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: typeid(e) vtable RTTI
#[test]
#[cfg(unix)]
fn m17_typeid() {
    let src = include_str!("fixtures/m17_typeid.cpp");
    let (out, _obj) = run_cpp_object_test("m17-typeid", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: volatile 成员函数
#[test]
#[cfg(unix)]
fn m17_volatile_member() {
    let src = include_str!("fixtures/m17_volatile_member.cpp");
    let (out, _obj) = run_cpp_object_test("m17-volatile-member", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

// =========================================================================
// M18: C++ 标准库扩展 (filesystem / thread / mutex / chrono / expected /
// flat_map / multi_index / regex / ranges / concepts / modules)
// =========================================================================

#[test]
#[cfg(unix)]
fn m18_chrono() {
    let src = include_str!("fixtures/m18_chrono.cpp");
    let (out, _obj) = run_cpp_object_test("m18-chrono", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_thread_mutex() {
    let src = include_str!("fixtures/m18_thread_mutex.cpp");
    let (out, _obj) = run_cpp_object_test("m18-thread-mutex", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_condition_variable() {
    let src = include_str!("fixtures/m18_condition_variable.cpp");
    let (out, _obj) = run_cpp_object_test("m18-cv", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_filesystem() {
    let src = include_str!("fixtures/m18_filesystem.cpp");
    let (out, _obj) = run_cpp_object_test("m18-filesystem", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_expected() {
    let src = include_str!("fixtures/m18_expected.cpp");
    let (out, _obj) = run_cpp_object_test("m18-expected", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_flat_map() {
    let src = include_str!("fixtures/m18_flat_map.cpp");
    let (out, _obj) = run_cpp_object_test("m18-flat-map", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_multi_index() {
    let src = include_str!("fixtures/m18_multi_index.cpp");
    let (out, _obj) = run_cpp_object_test("m18-multi-index", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_regex() {
    let src = include_str!("fixtures/m18_regex.cpp");
    let (out, _obj) = run_cpp_object_test("m18-regex", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_ranges() {
    let src = include_str!("fixtures/m18_ranges.cpp");
    let (out, _obj) = run_cpp_object_test("m18-ranges", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_concept() {
    let src = include_str!("fixtures/m18_concept.cpp");
    let (out, _obj) = run_cpp_object_test("m18-concept", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn m18_modules() {
    let src = include_str!("fixtures/m18_modules.cpp");
    let (out, _obj) = run_cpp_object_test("m18-modules", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

// ====== M19: 预处理器补齐 ======
// __COUNTER__ 单调递增, 跨宏嵌套仍保持唯一
#[test]
#[cfg(unix)]
fn m19_counter_monotonic() {
    let src = include_str!("fixtures/m19_counter.c");
    let (out, exe) = run_compile_and_run("m19-counter", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__COUNTER__ should produce 0,1,2");
    let _ = fs::remove_file(exe);
}

// __COUNTER__ 在宏展开中递增
#[test]
#[cfg(unix)]
fn m19_counter_in_function_macro() {
    let src = include_str!("fixtures/m19_counter_in_macro.c");
    let (out, exe) = run_compile_and_run("m19-counter-macro", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__COUNTER__ in macro should be 0,1,2");
    let _ = fs::remove_file(exe);
}

// __VA_OPT__: 非空时展开 content, 空时移除
#[test]
#[cfg(unix)]
fn m19_va_opt_empty_and_nonempty() {
    let src = include_str!("fixtures/m19_va_opt.c");
    let (out, exe) = run_compile_and_run("m19-va-opt", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__VA_OPT__ should expand to comma list when non-empty");
    let _ = fs::remove_file(exe);
}

// GCC 兼容预定义宏补齐
#[test]
#[cfg(unix)]
fn m19_predefined_macros_available() {
    let src = include_str!("fixtures/m19_predefined_macros.c");
    let (out, exe) = run_compile_and_run("m19-predef", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "all GCC predefined macros must be defined");
    let _ = fs::remove_file(exe);
}

// #pragma locale: 接受且不影响编译
#[test]
#[cfg(unix)]
fn m19_pragma_locale_accepted() {
    let src = include_str!("fixtures/m19_pragma_locale.c");
    let (out, exe) = run_compile_and_run("m19-pragma-locale", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(exe);
}

// ====== M23: Section 11 属性/builtin/alloca ======
// __attribute__((retain)): 变量和函数应被保留, gc-sections 不移除
#[test]
#[cfg(unix)]
fn m23_attr_retain_compiles_and_runs() {
    let src = include_str!("fixtures/m23_attr_retain.c");
    let (out, exe) = run_compile_and_run("m23-attr-retain", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "retain_var + retain_func(0) = 41+1 = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((used)): 静态函数即使未被引用也应保留并执行
#[test]
#[cfg(unix)]
fn m23_attr_used_compiles_and_runs() {
    let src = include_str!("fixtures/m23_attr_used.c");
    let (out, exe) = run_compile_and_run("m23-attr-used", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "used_helper(21) = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((flatten)): 解析且编译通过
#[test]
#[cfg(unix)]
fn m23_attr_flatten_compiles_and_runs() {
    let src = include_str!("fixtures/m23_attr_flatten.c");
    let (out, exe) = run_compile_and_run("m23-attr-flatten", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "outer(20) = (20+1)*2 = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((deprecated("msg"))): 仅声明, 解析/编译不应报错
#[test]
#[cfg(unix)]
fn m23_attr_deprecated_compiles_and_runs() {
    let src = include_str!("fixtures/m23_attr_deprecated.c");
    let (out, exe) = run_compile_and_run("m23-attr-deprecated", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42);
    let _ = fs::remove_file(exe);
}

// __attribute__((constructor(N))) / __attribute__((destructor(N))): 优先级排序
// GCC 约定: 数字越小优先级越高. 100 应在 300 之前构造.
#[test]
#[cfg(unix)]
fn m23_attr_ctor_priority_orders_correctly() {
    let src = include_str!("fixtures/m23_attr_ctor_priority.c");
    let (out, exe) = run_compile_and_run("m23-attr-ctor-priority", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    // init_100 (prio=100) 先, order=0*10+2=2; init_300 (prio=300) 后, order=2*10+1=21
    assert_eq!(rc, 21, "constructor priority 100 should run before 300");
    let _ = fs::remove_file(exe);
}

// __builtin_debugtrap / __builtin_assume_aligned / __builtin_alloca
#[test]
#[cfg(unix)]
fn m23_builtin_dbg_compiles_and_runs() {
    let src = include_str!("fixtures/m23_builtin_dbg.c");
    let (out, exe) = run_compile_and_run("m23-builtin-dbg", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    // alloca 4 个 int, 写 10/11/12/9, sum=42
    assert_eq!(rc, 42, "alloca + assume_aligned + (dead)debugtrap");
    let _ = fs::remove_file(exe);
}

// __attribute__((retain, used)) 联合使用
#[test]
#[cfg(unix)]
fn m23_attr_retain_used_compiles_and_runs() {
    let src = include_str!("fixtures/m23_attr_retain_used.c");
    let (out, exe) = run_compile_and_run("m23-attr-retain-used", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "combo_var + combo_fn(0) = 7+35 = 42");
    let _ = fs::remove_file(exe);
}
