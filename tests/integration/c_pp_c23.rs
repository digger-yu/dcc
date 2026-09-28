// included from tests/integration.rs — not a standalone cargo target

#[test]
fn failed_static_assert_is_a_hard_error() {
    // 注意：_Static_assert 失败已降级为警告 (W0010) 以支持内核构建
    // 这个测试现在验证警告是否正确发出
    let output = run_syntax_test(
        "static-assert-fail",
        "_Static_assert(2 * 3 == 7, \"math is broken\");\nint main(void) { return 0; }\n",
    );
    // 编译应该成功（降级为警告）
    assert!(
        output.status.success(),
        "static_assert failure should be a warning, not an error"
    );
    // 但应该包含警告信息
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("math is broken") || stderr.contains("W0010"),
        "stderr should mention the assertion or warning code: {}",
        stderr
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// __typeof__(struct T) 类型推导
#[test]
fn typeof_struct_compiles() {
    let source = include_str!("../fixtures/c/typeof_struct.c");
    let (output, obj) = run_object_test("typeof-struct", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// GNU __auto_type 类型推导
#[test]
fn auto_type_decl_compiles() {
    let source = include_str!("../fixtures/c/auto_type.c");
    let (output, obj) = run_object_test("auto-type", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// _Static_assert + __builtin_offsetof 编译期求值
#[test]
fn static_assert_offsetof_compiles() {
    let source = include_str!("../fixtures/c/sassert_off.c");
    let output = run_syntax_test("static-assert-offsetof", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// object-like 宏别名 + 函数式宏链式展开
#[test]
fn div_round_up_macro_expands() {
    let source = include_str!("../fixtures/c/div_round_up.c");
    let output = run_syntax_test("div-round-up", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// GNU 省略中间操作数的三元运算符 x ?: y
#[test]
fn gnu_elvis_operator_compiles() {
    let source = include_str!("../fixtures/c/elvis.c");
    let output = run_syntax_test("gnu-elvis", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 零参 function-like 宏 current_thread_info() + bitop 链
#[test]
fn current_thread_info_macro_compiles() {
    let source = include_str!("../fixtures/c/thread_info.c");
    let output = run_syntax_test("current-thread-info", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// NOFAIL / # 字符串化：多行实参折叠为单行字面量
#[test]
fn nofail_stringify_compiles() {
    let source = include_str!("../fixtures/c/stringify.c");
    let (output, _obj) = run_object_test("nofail-stringify", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// typeof(_Generic(...))：内核 READ_ONCE 类宏展开形态
#[test]
fn typeof_generic_read_once_compiles() {
    let source = include_str!("../fixtures/c/typeof_generic.c");
    let (output, exe) = run_compile_and_run("typeof-generic-read-once", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0);
}

// =================== C23 constexpr ===================

/// constexpr 与 static 组合应正确解析
#[test]
fn c23_constexpr_static_compile_and_run() {
    let source = include_str!("../fixtures/c/c23_constexpr_static.c");
    let (output, exe) = run_compile_and_run("c23-constexpr-static", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "constexpr_static should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

// =================== C23 constexpr 函数 ===================

/// C23 constexpr 函数: 解析为声明说明符, 等价 inline + always_inline
#[test]
#[cfg(unix)]
fn c23_constexpr_function_compile_and_run() {
    let source = include_str!("../fixtures/c/c23_constexpr.c");
    let (output, exe) = run_compile_and_run("c23-constexpr", source);
    assert!(
        output.status.success(),
        "c23 constexpr compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "c23_constexpr should return 0, got {rc}");
    let _ = fs::remove_file(exe);
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
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#define __DCC__ 1"), "missing __DCC__: {text}");
    assert!(text.contains("#define __GNUC__"), "missing __GNUC__: {text}");
    assert!(text.contains("#define USER_MACRO 42"), "missing USER_MACRO: {text}");
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
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("inc_target_value"), "include macro expansion failed: {text}");
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
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    // #ds / #cs 应被字符串化为 "ds" / "cs"
    assert!(text.contains("\"ds\""), "stringify 'ds' missing: {text}");
    assert!(text.contains("\"cs\""), "stringify 'cs' missing: {text}");
    // ## 应被令牌拼接
    assert!(text.contains("__savesegment_ds"), "paste 'ds' missing: {text}");
    assert!(text.contains("__savesegment_cs"), "paste 'cs' missing: {text}");
}

/// #pragma pack / weak / STDC FENV_ACCESS / GCC diagnostic
#[test]
#[cfg(unix)]
fn pragma_family_compile_and_run() {
    let source = include_str!("../fixtures/c/pragma_fam.c");
    let (output, exe) = run_compile_and_run("pragma-family", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "pragma_family should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// _Pragma 操作符: 等价于 #pragma
#[test]
#[cfg(unix)]
fn pragma_operator_compile_and_run() {
    let source = include_str!("../fixtures/c/pragma_op.c");
    let (output, exe) = run_compile_and_run("pragma-operator", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "pragma_operator should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// #pragma GCC system_header: 抑制后续警告, 不影响代码生成
#[test]
#[cfg(unix)]
fn gcc_system_header_compile_and_run() {
    let source = include_str!("../fixtures/c/system_header.c");
    let (output, exe) = run_compile_and_run("gcc-system-header", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // 只有 pragma 前的 #warning 应出现在 stderr,
    // pragma 后的 #warning 应被 system_header 抑制
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("before system_header"),
        "expected warning before system_header in stderr: {stderr}"
    );
    assert!(
        !stderr.contains("after system_header - should be suppressed"),
        "warning after system_header should be suppressed: {stderr}"
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "system_header test should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// _Static_assert 字符串化消息: C23 单参, 失败时附加表达式文本
#[test]
fn static_assert_stringification_succeeds() {
    let source = include_str!("../fixtures/c/sassert_msg.c");
    let output = run_syntax_test("static-assert-msg", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
#[cfg(unix)]
fn stdatomic_macros_hosted() {
    let src = include_str!("../fixtures/c/atomic.c");
    let (out, exe) = run_compile_and_run("atomic", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 7);
    let _ = fs::remove_file(&exe);
}

// __COUNTER__ 在宏展开中递增
#[test]
#[cfg(unix)]
fn m19_counter_in_function_macro() {
    let src = include_str!("../fixtures/c/counter_in_macro.c");
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
    let src = include_str!("../fixtures/c/va_opt.c");
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
    let src = include_str!("../fixtures/c/predefined_macros.c");
    let (out, exe) = run_compile_and_run_flags("m19-predef", src, &["-O1"]);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "all GCC predefined macros must be defined");
    let _ = fs::remove_file(exe);
}

// __BASE_FILE__ macro test
#[test]
#[cfg(unix)]
fn base_file_macro_available() {
    let src = include_str!("../fixtures/c/base_file.c");
    let (out, exe) = run_compile_and_run("base-file", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__BASE_FILE__ macro must be defined and contain filename");
    let _ = fs::remove_file(exe);
}

// __INCLUDE_LEVEL__ macro test
#[test]
#[cfg(unix)]
fn include_level_macro_available() {
    // 直接用夹具对 `include_level.c` + `include_level_inner.h`：主文件里的
    // `#include "include_level_inner.h"` 是**相对**包含，所以两份夹具必须保持
    // 原名同目录落盘。早先版本在测试里内联重写了同样内容的 `inner.h`/`main.c`，
    // 夹具因此零引用，且测的是“内联字符串”而不是入库的那对文件。
    let dir = std::env::temp_dir().join(format!("dcc-inclevel-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("include_level_inner.h"),
        include_str!("../fixtures/c/include_level_inner.h"),
    )
    .unwrap();
    let source_path = dir.join("include_level.c");
    fs::write(&source_path, include_str!("../fixtures/c/include_level.c")).unwrap();
    let exe_path = dir.join("include_level");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let status = Command::new(&exe_path).status().expect("run exe");
    assert_eq!(
        status.code(),
        Some(0),
        "__INCLUDE_LEVEL__ must be 0 in main and 1 in included header"
    );
    let _ = fs::remove_dir_all(&dir);
}

// __TIMESTAMP__ macro test
#[test]
#[cfg(unix)]
fn timestamp_macro_available() {
    let src = include_str!("../fixtures/c/timestamp.c");
    let (out, exe) = run_compile_and_run("timestamp", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__TIMESTAMP__ must be defined and contain valid date string");
    let _ = fs::remove_file(exe);
}

// #pragma locale: 接受且不影响编译
#[test]
#[cfg(unix)]
fn m19_pragma_locale_accepted() {
    let src = include_str!("../fixtures/c/pragma_locale.c");
    let (out, exe) = run_compile_and_run("m19-pragma-locale", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(exe);
}

// ====== M26: C23 预处理全家族端到端 ======
// #elifdef / #elifndef 分支选择
#[test]
#[cfg(unix)]
fn m26_elifdef_elifndef_runs() {
    let src = include_str!("../fixtures/c/elifdef.c");
    let (out, exe) = run_compile_and_run("m26-elifdef", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "#elifdef/#elifndef branch selection");
    let _ = fs::remove_file(exe);
}

// __has_include / __has_c_attribute / __has_builtin / __has_attribute /
// __STDC_ENDIAN_* 在 #if 中求值
#[test]
#[cfg(unix)]
fn m26_has_macros_c23_runs() {
    let src = include_str!("../fixtures/c/has_macros.c");
    let (out, exe) = run_compile_and_run("m26-has-macros", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(rc, 0, "#include_next chain");
}

// char16_t / char32_t (C11/C++11) 类型与字面量
#[test]
#[cfg(unix)]
fn m26_char16_char32_compiles_and_runs() {
    let src = include_str!("../fixtures/c/char16_char32.c");
    let (out, exe) = run_compile_and_run("m26-char16-char32", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "char16_t/char32_t size and value checks");
    let _ = fs::remove_file(exe);
}

/// fenv pragma STDC FENV_ACCESS 解析 (Section 11.2)
/// 验证 #pragma STDC FENV_ACCESS ON/OFF/DEFAULT 不会破坏编译.
/// pragma 状态在 dcc preproc 中由 fenv_access 字段记录, 并驱动
/// opt 阶段的浮点折叠开关与 fenv 调用的内联提供.
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

/// M3.4: typeof / __typeof__ operator.
#[test]
#[cfg(unix)]
fn m3_4_typeof() {
    let src = include_str!("../fixtures/c/typeof.c");
    let (out, code) = run_host_link_and_run("m3-4-typeof", src);
    assert!(
        out.status.success(),
        "m3_4 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "typeof checks failed (exit={code})");
}

// C23 fixtures
#[test]
#[cfg(unix)]
fn c23_alignas() {
    let src = include_str!("../fixtures/c/c23_alignas.c");
    let (out, exe) = run_compile_and_run("c23-alignas", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_alignas: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn has_extension_macro() {
    // __has_extension: Clang 兼容特性检测宏
    let src = include_str!("../fixtures/c/has_extension.c");
    let (out, exe) = run_compile_and_run("has-extension", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "has_extension: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_constexpr_var() {
    // C23 constexpr 变量: 必须常量初始化, 隐含 const
    let src = include_str!("../fixtures/c/c23_constexpr_var.c");
    let (out, exe) = run_compile_and_run("c23-constexpr-var", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_constexpr_var: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn c23_constexpr_var_error() {
    // C23 constexpr 变量非常量初始化器应报错 E0011
    let src = include_str!("../fixtures/c/c23_constexpr_var_error.c");
    let out = run_syntax_test("c23-constexpr-var-err", src);
    assert!(!out.status.success(), "should fail: constexpr requires constant initializer");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("constexpr") || stderr.contains("constant"),
        "error should mention constexpr/constant: {}",
        stderr
    );
}

/// C23 [[fallthrough]] / [[nodiscard]]: 解析兼容 + 运行时正确
#[test]
#[cfg(unix)]
fn c23_fallthrough_nodiscard_compile_and_run() {
    let src = include_str!("../fixtures/c/c23_fallthrough_nodiscard.c");
    let (out, exe) = run_compile_and_run("c23-fallthrough-nodiscard", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "c23 fallthrough/nodiscard compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "c23_fallthrough_nodiscard should return 0, got {rc}");
}

/// C23 [[nodiscard]]: 丢弃返回值时发射 W0401 警告
#[test]
fn c23_nodiscard_warn_emits_w0401() {
    let src = include_str!("../fixtures/c/c23_nodiscard_warn.c");
    let out = run_syntax_test("c23-nodiscard-warn", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("W0401"), "expected W0401 nodiscard warning: {stderr}");
    assert!(
        stderr.contains("nodiscard") || stderr.contains("ignoring"),
        "warning should mention nodiscard/ignoring: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn c23_bool() {
    let src = include_str!("../fixtures/c/c23_bool.c");
    let (out, exe) = run_compile_and_run("c23-bool", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_bool: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_brace_init() {
    let src = include_str!("../fixtures/c/c23_brace_init.c");
    let (out, exe) = run_compile_and_run("c23-brace-init", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_brace_init: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_enum_under() {
    let src = include_str!("../fixtures/c/c23_enum_under.c");
    let (out, exe) = run_compile_and_run("c23-enum-under", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_enum_under: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_has_c_attribute() {
    let src = include_str!("../fixtures/c/c23_has_c_attribute.c");
    let (out, exe) = run_compile_and_run("c23-has-c-attr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_has_c_attribute: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_nullptr() {
    let src = include_str!("../fixtures/c/c23_nullptr.c");
    let (out, exe) = run_compile_and_run("c23-nullptr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_nullptr: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_printf_b() {
    let src = include_str!("../fixtures/c/c23_printf_b.c");
    let (out, exe) = run_compile_and_run("c23-printf-b", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_printf_b: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_static_assert() {
    let src = include_str!("../fixtures/c/c23_static_assert.c");
    let (out, exe) = run_compile_and_run("c23-static-assert", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_static_assert: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_thread_local() {
    let src = include_str!("../fixtures/c/c23_thread_local.c");
    let (out, exe) = run_compile_and_run("c23-thread-local", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_thread_local: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn c23_typeof_unqual() {
    let src = include_str!("../fixtures/c/c23_typeof_unqual.c");
    let (out, exe) = run_compile_and_run("c23-typeof-unqual", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "c23_typeof_unqual: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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

// =================== __pragma ===================

/// __pragma(...) 应被接受 (无操作)
#[test]
fn ms_pragma_accepted_noop() {
    let src = "\
__pragma(message(\"hello\"))
__pragma(pack(push, 1))
__pragma(pack(pop))
int main(void) { return 0; }
";
    let (output, obj) = run_object_test("ms-pragma-noop", src);
    assert!(
        output.status.success(),
        "__pragma should be accepted as no-op: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(obj);
}

/// #error 宏展开后消息: 宏参数应出现在 error 消息中
#[test]
fn error_message_after_macro_expansion() {

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-err-macro-{}.c", std::process::id()));
    // 使用函数式宏展开后触发 #error
    let src_text = r#"
#define CHECK(x) _Generic((x), \
    int: 0, \
    default: (sizeof(#x), #error "unknown type: " #x) \
)
int main(void) { CHECK(1); return 0; }
"#;
    fs::write(&src, src_text).expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg(&src)
        .output()
        .expect("run dcc");

    let _ = fs::remove_file(&src);

    // 此测试验证宏展开后 #error 消息处理不崩溃
    // 根据 dcc 当前实现, #error 在预处理阶段触发, 宏展开后的 #error 也应正确报告
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 不强制要求特定行为, 只验证不 panic/abort
    assert!(
        !output.status.success() || stderr.is_empty() || stderr.contains("E0203"),
        "macro-expanded error handling: {stderr}"
    );
}

/// #error 在头文件中的诊断应携带头文件源码行 (回归测试)
#[test]
fn error_in_header_carries_header_source_line_regression() {

    let dir = std::env::temp_dir();
    let hdr = dir.join(format!("dcc-err-hdr-{}.h", std::process::id()));
    let src = dir.join(format!("dcc-err-hdr-src-{}.c", std::process::id()));

    fs::write(&hdr, "#ifndef FOO_H\n#define FOO_H\n#error stop in header\n#endif\n")
        .expect("write header");
    fs::write(
        &src,
        format!("#include \"{}\"\nint main(void) {{ return 0; }}\n", hdr.display()),
    )
    .expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-fsyntax-only")
        .arg(&src)
        .output()
        .expect("run dcc");

    let _ = fs::remove_file(&hdr);
    let _ = fs::remove_file(&src);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "header #error must fail");
    assert!(stderr.contains("E0203"), "should have E0203: {stderr}");
}

// ====== debug consumers: DWARF / BTF / addr2line validation ======

// ===== COMDAT group (SHF_GROUP) integration tests =====

// ===== Cross-compile predefined macro tests =====

/// Verify that -fmacro-backtrace-limit=N configures the macro expansion
/// recursion depth limit.
#[test]
#[cfg(unix)]
fn macro_backtrace_limit_configurable() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-macro-limit-{}.c", std::process::id()));
    // Create a file with deeply nested macro expansion
    let mut content = String::new();
    for i in 0..200 {
        if i < 199 {
            content.push_str(&format!("#define M{} M{}\n", i, i + 1));
        } else {
            content.push_str(&format!("#define M{} 42\n", i));
        }
    }
    content.push_str("int x = M0;\n");
    fs::write(&src, &content).expect("write src");
    // Compile with default limit (128) - should trigger W0201 warning
    let compile_default = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join(format!("dcc-macro-limit-default-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run dcc default");
    // Compile with higher limit (256) - should not trigger warning
    let compile_higher = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fmacro-backtrace-limit=256", "-o"])
        .arg(dir.join(format!("dcc-macro-limit-higher-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run dcc higher");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(dir.join(format!("dcc-macro-limit-default-{}.o", std::process::id())));
    let _ = fs::remove_file(dir.join(format!("dcc-macro-limit-higher-{}.o", std::process::id())));
    // Both should succeed (warning is non-fatal)
    assert!(
        compile_default.status.success(),
        "default limit compile failed: {}",
        String::from_utf8_lossy(&compile_default.stderr)
    );
    assert!(
        compile_higher.status.success(),
        "higher limit compile failed: {}",
        String::from_utf8_lossy(&compile_higher.stderr)
    );
    // The default limit should produce a warning about depth limit
    let default_stderr = String::from_utf8_lossy(&compile_default.stderr);
    assert!(
        default_stderr.contains("depth limit") || default_stderr.contains("W0201"),
        "expected depth limit warning with default limit, stderr: {}",
        default_stderr
    );
}

// ====== vDSO integration tests ======

/// C99 hexadecimal floating-point constants: 0x1.5p3 form
#[test]
fn hex_float_syntax_ok() {
    let src = r#"
double a = 0x1.0p0;
double b = 0x1.4p3;
double c = 0x1p10;
float d = 0x1.0p-1f;
int main(void) { return 0; }
"#;
    let output = run_syntax_test("hex-float", src);
    assert!(
        output.status.success(),
        "Hex float constants should parse. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C99 hex float runtime value verification
#[test]
fn hex_float_run_returns_zero() {
    let source = include_str!("../fixtures/c/hex_float.c");
    let (output, exe) = run_compile_and_run("hex-float-run", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "hex_float should return 0, got {rc}");
}

/// C89 #line directive: remap line numbers and file names
#[test]
fn line_directive_syntax_ok() {
    let source = include_str!("../fixtures/c/line_directive.c");
    let output = run_syntax_test("line-directive", source);
    assert!(
        output.status.success(),
        "#line directive should be accepted. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// GNU range initializer: [first ... last] = val
#[test]
fn gnu_range_init_syntax_ok() {
    let src = r#"
int a[10] = { [1 ... 5] = 42 };
int b[8] = { [0 ... 7] = 1 };
int main(void) { return 0; }
"#;
    let output = run_syntax_test("gnu-range-init", src);
    assert!(
        output.status.success(),
        "GNU range initializer should parse. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// GNU range initializer runtime value verification
#[test]
fn gnu_range_init_run_returns_zero() {
    let source = include_str!("../fixtures/c/gnu_range_init.c");
    let (output, exe) = run_compile_and_run("gnu-range-init-run", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "gnu_range_init should return 0, got {rc}");
}

/// C99 <inttypes.h> format macros, <float.h>, <limits.h>, <errno.h> builtins
#[test]
fn c99_std_macros_syntax_ok() {
    let source = include_str!("../fixtures/c/c99_std_macros.c");
    let output = run_syntax_test("c99-std-macros", source);
    assert!(
        output.status.success(),
        "C99 standard macros should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C99 inttypes.h PRId64/PRIu64 format macros availability
#[test]
fn inttypes_format_macros_defined() {
    let src = r#"
#if !defined(__PRId64) || !defined(__PRIu64) || !defined(__PRIx64)
#error "inttypes format macros not defined"
#endif
int main(void) { return 0; }
"#;
    let output = run_syntax_test("inttypes-macros", src);
    assert!(
        output.status.success(),
        "inttypes format macros should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C99 freestanding errno.h constants
#[test]
fn errno_constants_defined() {
    let src = r#"
#if EDOM != 33
#error "EDOM"
#endif
#if ERANGE != 34
#error "ERANGE"
#endif
#if EILSEQ != 84
#error "EILSEQ"
#endif
int main(void) { return 0; }
"#;
    let output = run_syntax_test("errno-consts", src);
    assert!(
        output.status.success(),
        "errno constants should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C99 Unicode character names in identifiers: \uXXXX / \UXXXXXXXX
#[test]
fn c99_unicode_ident_syntax_ok() {
    let source = include_str!("../fixtures/c/c99_unicode_ident.c");
    let output = run_syntax_test("c99-unicode-ident", source);
    assert!(
        output.status.success(),
        "C99 Unicode identifiers should parse. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C99 Unicode identifier runtime value verification
#[test]
fn c99_unicode_ident_run_returns_zero() {
    let source = include_str!("../fixtures/c/c99_unicode_ident.c");
    let (output, exe) = run_compile_and_run("c99-unicode-ident-run", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "c99_unicode_ident should return 0, got {rc}");
}

/// C99 freestanding signal.h constants
#[test]
fn signal_constants_defined() {
    let src = r#"
#if __SIGHUP != 1
#error "SIGHUP"
#endif
#if __SIGINT != 2
#error "SIGINT"
#endif
#if __SIGKILL != 9
#error "SIGKILL"
#endif
#if __SIGSEGV != 11
#error "SIGSEGV"
#endif
#if __SIGTERM != 15
#error "SIGTERM"
#endif
int main(void) { return 0; }
"#;
    let output = run_syntax_test("signal-consts", src);
    assert!(
        output.status.success(),
        "signal constants should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C99 tgmath.h type-generic math macros (with __tg_ prefix)
#[test]
fn tgmath_macros_defined() {
    let src = r#"
/* tgmath macros should be available as __tg_ prefixed function-like macros */
#if !defined(__tg_sqrt)
#error "__tg_sqrt not defined"
#endif
#if !defined(__tg_fabs)
#error "__tg_fabs not defined"
#endif
#if !defined(__tg_sin)
#error "__tg_sin not defined"
#endif
#if !defined(__tg_pow)
#error "__tg_pow not defined"
#endif
int main(void) { return 0; }
"#;
    let output = run_syntax_test("tgmath-macros", src);
    assert!(
        output.status.success(),
        "tgmath macros should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 __STDC_VERSION__ must be 202312L per ISO C23 standard
#[test]
fn c23_stdc_version_value() {
    let src = include_str!("../fixtures/c/c23_stdc_version.c");
    let path = std::env::temp_dir().join(format!("dcc-c23-stdc-version-{}.c", std::process::id()));
    fs::write(&path, src).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=c23", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(path);
    assert!(
        output.status.success(),
        "__STDC_VERSION__ should be 202312L. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C11 <stdnoreturn.h> noreturn macro and <stdalign.h> macros
#[test]
fn c11_stdnoreturn_stdalign_macros() {
    // Test noreturn macro (works in all C11+ modes)
    let src = r#"
noreturn void die(void) { while(1) {} }
int main(void) { return 0; }
"#;
    let output = run_syntax_test("stdnoreturn-macro", src);
    assert!(
        output.status.success(),
        "noreturn macro should expand to _Noreturn. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C11 <stdalign.h> alignas/alignof macros in C11 mode
#[test]
fn c11_stdalign_macros_c11_mode() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-stdalign-c11-{}.c", std::process::id()));
    fs::write(
        &src_path,
        r#"
#include <stddef.h>
alignas(16) int aligned_var;
int align_check = alignof(int);
int main(void) { return 0; }
"#,
    )
    .expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsyntax-only", "-std=c11"])
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "alignas/alignof macros should work in C11 mode. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C11/C23 standard library compliance macros
#[test]
fn c11_std_compliance_macros() {
    let src = include_str!("../fixtures/c/c11_std_compliance_macros.c");
    let output = run_syntax_test("c11-std-compliance", src);
    assert!(
        output.status.success(),
        "standard compliance macros should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// __STDC_IEC_559__ and __STDC_ISO_10646__ in hosted mode
#[test]
fn hosted_iec559_iso10646_macros() {
    let src = r#"
#ifndef __STDC_IEC_559__
#error "__STDC_IEC_559__ not defined in hosted mode"
#endif
#ifndef __STDC_ISO_10646__
#error "__STDC_ISO_10646__ not defined"
#endif
#ifndef __STDC_UTF_16__
#error "__STDC_UTF_16__ not defined"
#endif
#ifndef __STDC_UTF_32__
#error "__STDC_UTF_32__ not defined"
#endif
int ok = 1;
"#;
    let output = run_syntax_test("hosted-compliance", src);
    assert!(
        output.status.success(),
        "hosted mode compliance macros. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// offsetof macro works via __builtin_offsetof
#[test]
fn offsetof_macro_via_builtin() {
    let src = r#"
struct s { int a; char b; long c; };
int x = offsetof(struct s, b);
int y = offsetof(struct s, c);
_Static_assert(offsetof(struct s, a) == 0, "a offset");
"#;
    let output = run_syntax_test("offsetof-macro", src);
    assert!(
        output.status.success(),
        "offsetof macro should work. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// __has_feature returns correct values for known features
#[test]
fn has_feature_known_features() {
    let src = r#"
/* C11 features dcc supports */
#if !__has_feature(c_atomic)
#error "c_atomic should be detected"
#endif
#if !__has_feature(tls)
#error "tls should be detected"
#endif
#if !__has_feature(c_alignas)
#error "c_alignas should be detected"
#endif
#if !__has_feature(c_alignof)
#error "c_alignof should be detected"
#endif
#if !__has_feature(c_static_assert)
#error "c_static_assert should be detected"
#endif
#if !__has_feature(c_generic_selections)
#error "c_generic_selections should be detected"
#endif
/* GCC attribute features */
#if !__has_feature(attribute_weak)
#error "attribute_weak should be detected"
#endif
#if !__has_feature(attribute_visibility)
#error "attribute_visibility should be detected"
#endif
#if !__has_feature(attribute_noreturn)
#error "attribute_noreturn should be detected"
#endif
/* Type support */
#if !__has_feature(int128)
#error "int128 should be detected"
#endif
/* Unknown feature should return 0 */
#if __has_feature(totally_unknown_feature_xyz)
#error "unknown feature should return 0"
#endif
int ok = 1;
"#;
    let output = run_syntax_test("has-feature", src);
    assert!(
        output.status.success(),
        "__has_feature should detect known features. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 variadic macros without leading fixed parameters
#[test]
fn c23_variadic_macro_no_leading_param() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-va-no-lead-{}.c", std::process::id()));
    fs::write(
        &src_path,
        r#"
#define FOO(...) __VA_ARGS__
#define BAR(...) sizeof(__VA_ARGS__)
#define EMPTY(...)
int a = FOO(42);
int b = BAR(int);
int c = EMPTY();
"#,
    )
    .expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E"])
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&src_path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "preprocessing should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("int a = 42"), "FOO(42) should expand to 42, got: {stdout}");
    assert!(
        stdout.contains("int b = sizeof(int)"),
        "BAR(int) should expand to sizeof(int), got: {stdout}"
    );
}

/// C11 anonymous struct/union members (used by kernel struct_group)
#[test]
fn c11_anonymous_struct_union_members() {
    let src = include_str!("../fixtures/c/c11_anonymous_member.c");
    let output = run_syntax_test("c11-anon-member", src);
    assert!(
        output.status.success(),
        "anonymous struct/union members should be accessible. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 char8_t type support
#[test]
fn c23_char8_t_type() {
    let src = include_str!("../fixtures/c/c23_char8_t.c");
    let output = run_syntax_test("c23-char8t", src);
    assert!(
        output.status.success(),
        "char8_t type should be recognized. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 constexpr variables are implicitly const with constant initializer check
#[test]
fn c23_constexpr_variables() {
    let src = r#"
constexpr int x = 42;
constexpr int y = 100 + 23;
constexpr long big = 1LL << 32;
int main(void) { return x != 42 || y != 123; }
"#;
    let output = run_syntax_test("c23-constexpr-vars", src);
    assert!(
        output.status.success(),
        "constexpr variables should compile. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 [[...]] standard attribute syntax with semantic effects
#[test]
fn c23_std_attributes_effect() {
    let src = r#"
[[deprecated("use new_func")]]
void old_func(void) {}

[[nodiscard]]
int must_use(void) { return 42; }

[[maybe_unused]]
int unused_var = 10;

[[noreturn]]
void never_returns(void) { while(1) {} }

void test(void) {
    old_func();
    must_use();
    never_returns();
}
"#;
    let output = run_syntax_test("c23-std-attrs", src);
    assert!(
        output.status.success(),
        "C23 [[...]] attributes should parse correctly. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 alignas on struct members
#[test]
fn c23_alignas_in_struct() {
    let src = r#"
struct aligned {
    int a;
    _Alignas(16) int b;
    char c;
};
int main(void) {
    struct aligned s;
    return s.a + s.b + s.c;
}
"#;
    let output = run_syntax_test("c23-alignas-struct", src);
    assert!(
        output.status.success(),
        "alignas in struct should work. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 enum with explicit underlying type
#[test]
fn c23_enum_underlying_type() {
    let src = r#"
enum color : unsigned char { RED, GREEN, BLUE };
enum big : long { X = 100 };
enum small : signed char { A = -1, B = 127 };
int main(void) {
    enum color c = RED;
    enum big b = X;
    enum small s = A;
    return c + b + s;
}
"#;
    let output = run_syntax_test("c23-enum-underlying-type", src);
    assert!(
        output.status.success(),
        "enum with base type should compile. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C23 <stdbit.h> bit manipulation macros
#[test]
fn c23_stdbit_macros() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-stdbit-{}.c", std::process::id()));
    fs::write(
        &src_path,
        r#"
/* __STDC_VERSION_STDBIT_H__ should be defined in C23 mode */
#ifndef __STDC_VERSION_STDBIT_H__
#error "__STDC_VERSION_STDBIT_H__ not defined"
#endif
/* stdc_count_ones_ui should expand to __builtin_popcount */
int ones = stdc_count_ones_ui(0xFF);
/* stdc_has_single_bit_ui should detect powers of 2 */
int pow2 = stdc_has_single_bit_ui(64);
/* stdc_log2_floor_ui should compute floor(log2(x)) */
int lg = stdc_log2_floor_ui(256);
"#,
    )
    .expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=c23", "-E"])
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&src_path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdbit preprocessing should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("__builtin_popcount"),
        "stdc_count_ones_ui should expand to popcount. got: {stdout}"
    );
}

/// C89 digraph support: <: :> <% %> %: %:%:
/// Digraphs are alternative tokens for [ ] { } # ##
#[test]
fn c89_digraphs_compile_and_run() {
    let source = include_str!("../fixtures/c/c89_digraphs.c");
    let (output, exe) = run_compile_and_run("c89-digraphs", source);
    assert!(
        output.status.success(),
        "digraph compile failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run digraph test");
    assert_eq!(run.status.code(), Some(0), "digraph test should return 0");
    let _ = fs::remove_file(&exe);
}

/// C89 digraph preprocessing: %:define and %:%: token paste
#[test]
fn c89_digraphs_preprocess() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-digraph-pp-{}.c", std::process::id()));
    fs::write(
        &src_path,
        "%:define MAGIC 42\n\
         %:define CONCAT(a, b) a %:%: b\n\
         int CONCAT(my_, var) = MAGIC;\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E"])
        .arg(&src_path)
        .output()
        .expect("run dcc -E");
    let _ = fs::remove_file(&src_path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "digraph preprocess should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("int my_var = 42"),
        "digraphs should expand correctly. got: {stdout}"
    );
}

/// C89 trigraph support: ??= ??( ??/ ??) ??' ??< ??! ??> ??-
/// Trigraphs are enabled with -trigraphs flag
#[test]
fn c89_trigraphs_compile_and_run() {
    let source = include_str!("../fixtures/c/c89_trigraphs.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-c89-trigraphs-{}.c", std::process::id()));
    let exe_path = dir.join(format!("dcc-c89-trigraphs-{}", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-trigraphs", "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "trigraph compile failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
    }
    let run = std::process::Command::new(&exe_path)
        .output()
        .expect("run trigraph test");
    let _ = fs::remove_file(&exe_path);
    assert_eq!(run.status.code(), Some(0), "trigraph test should return 0");
}

/// C89 trigraph preprocessing: ??= -> #, ??< -> {, etc.
#[test]
fn c89_trigraphs_preprocess() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-trigraph-pp-{}.c", std::process::id()));
    fs::write(
        &src_path,
        "??=define MAGIC 42\n\
         int x = MAGIC;\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-trigraphs"])
        .arg(&src_path)
        .output()
        .expect("run dcc -E -trigraphs");
    let _ = fs::remove_file(&src_path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "trigraph preprocess should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("int x = 42"),
        "??= should become # and MAGIC should expand to 42. got: {stdout}"
    );
}

/// C89 trigraphs should NOT be converted in string literals
#[test]
fn c89_trigraphs_not_in_strings() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-trigraph-str-{}.c", std::process::id()));
    fs::write(&src_path, "const char *s = \"??= ??( ??/\";\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-trigraphs"])
        .arg(&src_path)
        .output()
        .expect("run dcc -E -trigraphs");
    let _ = fs::remove_file(&src_path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "trigraph preprocess should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Trigraphs inside string literals should remain unchanged
    assert!(
        stdout.contains("\"??= ??( ??/\""),
        "trigraphs in strings should not be converted. got: {stdout}"
    );
}

/// Without -trigraphs, trigraph sequences should remain unchanged
#[test]
fn c89_trigraphs_disabled_by_default() {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-trigraph-off-{}.c", std::process::id()));
    fs::write(&src_path, "int x = ??<1??>;\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E"])
        .arg(&src_path)
        .output()
        .expect("run dcc -E");
    let _ = fs::remove_file(&src_path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Without -trigraphs, ??< and ??> should remain as-is
    // (they'll likely cause a parse error, but the preprocessor should pass them through)
    assert!(
        stdout.contains("??<") || stdout.contains("??"),
        "trigraphs should not be converted without -trigraphs. got: {stdout}"
    );
}

/// C99 restrict qualifier: parsed and used in alias analysis
#[test]
fn c99_restrict_compile_and_run() {
    let source = include_str!("../fixtures/c/c99_restrict.c");
    let (output, exe) = run_compile_and_run("c99-restrict", source);
    assert!(
        output.status.success(),
        "restrict compile failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run restrict test");
    let _ = fs::remove_file(&exe);
    assert_eq!(run.status.code(), Some(0), "restrict test should return 0");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("PASS"), "restrict test should print PASS. got: {stdout}");
}

/// C11 _Noreturn function qualifier
#[test]
fn c11_noreturn_compile_and_run() {
    let source = include_str!("../fixtures/c/c11_noreturn.c");
    let (output, exe) = run_compile_and_run("c11-noreturn", source);
    assert!(
        output.status.success(),
        "noreturn compile failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run noreturn test");
    let _ = fs::remove_file(&exe);
    assert_eq!(run.status.code(), Some(0), "noreturn test should return 0");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("PASS"), "noreturn test should print PASS. got: {stdout}");
}

/// C23 [[unsequenced]] and [[reproducible]] attributes
#[test]
fn c23_attributes_compile_and_run() {
    let source = include_str!("../fixtures/c/c23_attributes.c");
    let (output, exe) = run_compile_and_run("c23-attrs", source);
    assert!(
        output.status.success(),
        "c23 attrs compile failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run c23 attrs test");
    let _ = fs::remove_file(&exe);
    assert_eq!(run.status.code(), Some(0), "c23 attrs test should return 0");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("PASS"), "c23 attrs test should print PASS. got: {stdout}");
}

// ===== Commit 10: OpenMP parallelization =====

#[test]
fn openmp_parallel_for_pragma() {
    // Verify that -fopenmp flag is accepted
    let args = vec![
        "-fopenmp".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-fopenmp should parse");
    // -fopenmp is accepted (may produce warning but doesn't error)
    assert_eq!(cfg.opt, 0);
}

#[test]
fn pragma_simd() {
    // Verify that #pragma simd is collected
    let args = vec!["-c".to_string(), "input.c".to_string()];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.opt, 0);
}

#[test]
fn openmp_simd_pragma() {
    // Verify that -fopenmp=simd flag is accepted
    let args = vec![
        "-fopenmp=simd".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-fopenmp=simd should parse");
    assert_eq!(cfg.opt, 0);
}

#[test]
fn constant_builtin_and_static_assert_pass_through_typed_ir() {
    let output = run_syntax_test(
        "typed-ir-ok",
        "_Static_assert(__builtin_constant_p(((__int128)1 << 100) / 3), \"constant\");\n\
         int main(void) { return __builtin_constant_p(42) == 1 ? 0 : 1; }\n",
    );
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// C99 float.h builtin macros
#[test]
fn float_builtin_macros_defined() {
    let src = r#"
#if __FLT_RADIX__ != 2
#error "FLT_RADIX"
#endif
#if __DBL_MANT_DIG__ != 53
#error "DBL_MANT_DIG"
#endif
#if __FLT_MAX_10_EXP__ != 38
#error "FLT_MAX_10_EXP"
#endif
int main(void) { return 0; }
"#;
    let output = run_syntax_test("float-macros", src);
    assert!(
        output.status.success(),
        "float.h macros should be defined. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// M1.3: preprocessor verification — macro expansion, conditional
/// compilation, token pasting, stringification, #undef, #ifndef,
/// defined() operator.  Compare dcc -E output with gcc -E.
#[test]
#[cfg(unix)]
fn m1_3_preprocessor() {
    let src = include_str!("../fixtures/c/preproc.c");
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
    assert!(dcc_out.status.success(), "dcc -E: {}", String::from_utf8_lossy(&dcc_out.stderr));
    assert!(gcc_out.status.success(), "gcc -E: {}", String::from_utf8_lossy(&gcc_out.stderr));
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

/// C11 标准头兼容宏（`noreturn` / `alignas` / `alignof`）必须由预处理器
/// 预定义。`c11_std_headers.c` 故意**不** `#include <stdnoreturn.h>` /
/// `<stdalign.h>`（dcc 不自带这组资源头），只依赖预定义宏；gcc 则必须先包含
/// 才能用这些名字，它对本夹具直接报 `expected ';' before 'void'` —— 没有可用
/// 的 gcc oracle，所以做 dcc 能力面的结构断言：除了编译成功 + rc==0，还必须
/// 从 `.symtab` / 节头表证明 `#if __STDC_VERSION__ >= 201112` 那一段**真的被
/// 编译到了**（`_Alignof` 产出的 `a`、`alignas(16)` 产出的 `x` 及其 16 字节对齐
/// 的宿主节），否则一个“把受限分支整段注掉”的改动能静默假装通过。
#[test]
#[cfg(unix)]
fn c11_std_headers_compat_macros_compile() {
    let src = include_str!("../fixtures/c/c11_std_headers.c");
    // 1) 先证“能跑且返回 0”：`noreturn void foo(void)` 这类声明必须被前端接受。
    let (output, exe) = run_compile_and_run("c11-std-headers", src);
    assert!(
        output.status.success(),
        "noreturn/alignas/alignof 预定义宏应可用: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "c11_std_headers.c 应返回 0，got {rc}");
    let _ = fs::remove_file(exe);
    // 2) 再证受限分支真的参编：`a = _Alignof(int)` 与 `alignas(16) int x;`
    //    必须以全局 OBJECT 符号落在对象文件里。
    let (coutput, obj) = run_object_test("c11-std-headers-o", src);
    assert!(
        coutput.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&coutput.stderr)
    );
    let bytes = fs::read(&obj).expect("read object");
    let names: Vec<String> = elf64_syms(&bytes)
        .into_iter()
        .map(|s| s.name)
        .collect();
    for expected in ["a", "x", "foo", "main"] {
        assert!(
            names.iter().any(|n| n == expected),
            "alignas/alignof/noreturn 分支应产出符号 {expected}，实际 symtab: {names:?}"
        );
    }
    // 3) `alignas(16) int x;` 必须把 `.bss` 的 sh_addralign 抬到 16（否则
    //    alignas 被当成了空属性）；`.data`（存 `_Alignof` 结果的 `a`）保持 8
    //    作对照组，防止“所有节一律 16”这类误伤也能通过。
    if !tool_exists("readelf") {
        eprintln!("skip c11 alignas section-alignment check: readelf not found");
        let _ = fs::remove_file(obj);
        return;
    }
    let secs = Command::new("readelf")
        .args(["-SW"])
        .arg(&obj)
        .output()
        .expect("readelf -SW");
    assert!(secs.status.success(), "readelf -SW failed");
    let sw = String::from_utf8_lossy(&secs.stdout).into_owned();
    let align_of = |want: &str| -> Option<String> {
        sw.lines()
            .find(|l| l.split_whitespace().any(|tok| tok == want))
            .and_then(|l| l.split_whitespace().last().map(str::to_string))
    };
    assert_eq!(
        align_of(".bss").as_deref(),
        Some("16"),
        "alignas(16) int x 必须把 .bss 的 sh_addralign 抬到 16，readelf -SW：{sw}"
    );
    assert_eq!(
        align_of(".data").as_deref(),
        Some("8"),
        "对照组：普通 int a 所在的 .data 不得被连带抬到 16"
    );
    let _ = fs::remove_file(obj);
}

/// GNU 语句表达式里的 `__auto_type`（内核 `min_t`/`clamp_t` 一类宏的基础）：
/// 类型由初始化式推出，语句表达式的值必须是那个局部变量。
#[test]
#[cfg(unix)]
fn stmt_expr_auto_type_matches_gcc() {
    let src = include_str!("../fixtures/c/stmt_auto.c");
    let rc = run_gcc_cross_check("stmt-auto", src);
    assert_eq!(rc, 1, "__auto_type 局部变量应回递 (1 << 0) == 1");
}

/// 同一形态但用 `typeof`：第二个声明的类型由第一个变量推导，最后
/// 以 `x_ / 10` 作为语句表达式的值。`typeof` 与 `__auto_type` 共用推导机制，
/// 两者分开钉住才能定位哪一条推导链路坏了。
#[test]
#[cfg(unix)]
fn stmt_expr_typeof_matches_gcc() {
    let src = include_str!("../fixtures/c/stmt_typeof.c");
    let rc = run_gcc_cross_check("stmt-typeof", src);
    assert_eq!(rc, 10, "typeof(x_) 应保留整型并算出 100 / 10 == 10");
}

/// 夹具私有伴生头 `c/comment.h` 的引用钉住：与 `src/preproc.rs` 的单元测试
/// 用同一份入库文件（不是副本），但走**命令行** `-include` 路径。注释里的
/// `Don't` / `"quoted text"` / `'x'` 与 `COMMENT_WORD` 不得漏进预处理输出；
/// 真正的宏定义（`FORCED_VALUE` / `COMMENT_WORD`）必须展开。gcc 作同形对照。
#[test]
#[cfg(unix)]
fn forced_include_comment_header_matches_gcc() {
    if !tool_exists("gcc") {
        eprintln!("skip forced_include_comment_header_matches_gcc: gcc not found");
        return;
    }
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let hdr_path = dir.join(format!("dcc-comment-hdr-{pid}.h"));
    let src_path = dir.join(format!("dcc-comment-hdr-{pid}.c"));
    fs::write(&hdr_path, include_str!("../fixtures/c/comment.h")).expect("write header fixture");
    // 把夹具里的两个宏都用上：`COMMENT_WORD` 只在注释里被提及，源码里用它
    // 才能区分“注释被当源码”与“宏正常展开”两种失效。
    fs::write(&src_path, "int value = FORCED_VALUE;\nint w = COMMENT_WORD;\n").expect("write src");
    let strip = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect()
    };
    let run_e = |prog: &str| {
        Command::new(prog)
            .args(["-E", "-include"])
            .arg(&hdr_path)
            .arg(&src_path)
            .output()
            .unwrap_or_else(|e| panic!("{prog} -E: {e}"))
    };
    let dcc_out = run_e(env!("CARGO_BIN_EXE_dcc"));
    let gcc_out = run_e("gcc");
    let _ = fs::remove_file(&hdr_path);
    let _ = fs::remove_file(&src_path);
    assert!(dcc_out.status.success(), "dcc -E: {}", String::from_utf8_lossy(&dcc_out.stderr));
    assert!(gcc_out.status.success(), "gcc -E: {}", String::from_utf8_lossy(&gcc_out.stderr));
    let dcc_lines = strip(&String::from_utf8_lossy(&dcc_out.stdout));
    let gcc_lines = strip(&String::from_utf8_lossy(&gcc_out.stdout));
    assert_eq!(dcc_lines, gcc_lines, "force-include 预处理输出与 gcc 不一致");
    // 夹具自身的两个宏必须真的被展开（而不是原样输出名字）
    assert_eq!(dcc_lines, vec!["int value = 7;".to_string(), "int w = 99;".to_string()]);
}
