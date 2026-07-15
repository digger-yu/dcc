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
