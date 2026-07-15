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
        asm_text.contains(".ascii \"->SIZE_usb_device_id 8 sizeof(struct usb_device_id)\""),
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
