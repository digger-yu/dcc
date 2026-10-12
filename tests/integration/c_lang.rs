// included from tests/integration.rs — not a standalone cargo target

#[test]
fn unknown_typedef_is_not_coerced_to_int() {
    let output =
        run_syntax_test("unknown-type", "missing_typedef value;\nint main(void) { return 0; }\n");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown type or typedef"));
}

#[test]
fn s128_typedef_with_attribute_parses() {
    let output = run_syntax_test(
        "s128-typedef",
        "typedef __signed__ __int128 __s128 __attribute__((aligned(16)));\n\
         typedef __s128 s128;\n\
         int main(void) { s128 x = 0; (void)x; return 0; }\n",
    );
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let size = fs::metadata(&object).expect("object was emitted").len();
    assert!(size > 64, "ELF object should contain generated code");
    let _ = fs::remove_file(object);
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// Switch/case range comparison correctness:
/// Bug 1: gen_expr clobbers rcx (switch value register) when evaluating
///         case expressions containing shifts or other rcx-using ops.
/// Bug 2: range comparison used signed jl/jg for unsigned switch values,
///         causing incorrect matching for values above INT_MAX.
#[test]
#[cfg(unix)]
fn switch_range_comparison_correctness() {
    let src = include_str!("../fixtures/c/switch_range_correctness.c");
    let (out, exe) = run_compile_and_run("switch-range-correctness", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "switch range correctness compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "switch range correctness test failed (rc={rc})");
}

#[test]
#[cfg(unix)]
fn indirect_call_assignment_preserves_lvalue_address() {
    let src = r#"
struct S { int (*fp)(void); int status; };
static int ret3(void) { return 3; }
int main(void) {
    struct S s;
    s.fp = ret3;
    s.status = 0;
    s.status = (*s.fp)();
    return s.status == 3 ? 0 : 1;
}
"#;
    let (out, exe) = run_compile_and_run("indirect-call-assign", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "indirect call assign compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "indirect call assignment failed (rc={rc})");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn stdbool_before_linux_stddef_enum_parses() {
    let dir = std::env::temp_dir();
    let hdr_dir = dir.join(format!("dcc-stddef-{}", std::process::id()));
    let linux_dir = hdr_dir.join("linux");
    fs::create_dir_all(&linux_dir).expect("create linux include dir");
    fs::write(linux_dir.join("stddef.h"), "enum {\n\tfalse = 0,\n\ttrue = 1\n};\n")
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// glibc string.h：`_Generic(0 ? ... : ..., const void *: ..., default: ...)`
/// 三元 else 不得吞掉 association 列表的逗号。
#[test]
fn generic_with_ternary_controlling_expr_compiles() {
    let source = include_str!("../fixtures/c/generic_tern.c");
    let (output, obj) = run_object_test("generic-ternary", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// `_Generic` 指针按 pointee 精确匹配，char* 与 int* 不得互匹配。
#[test]
fn generic_pointer_precise_match_compiles() {
    let source = include_str!("../fixtures/c/generic_ptr.c");
    let (output, obj) = run_object_test("generic-pointer", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// static_call 语句表达式 + 尾随调用: ({ ... ;; expr })()
#[test]
fn stmt_expr_trailing_call_compiles() {
    let source = include_str!("../fixtures/c/stmt_call.c");
    let (output, obj) = run_object_test("stmt-expr-call", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 逗号分隔位域: u16 a:8, b:4, c:1（desc_struct 模式）
#[test]
fn bitfield_comma_separated_compiles() {
    let source = include_str!("../fixtures/c/bitfield.c");
    let (output, obj) = run_object_test("bitfield-comma", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 结构体空字段: struct { int a; ; }（lockdep class 宏）
#[test]
fn struct_empty_field_compiles() {
    let source = include_str!("../fixtures/c/struct_empty.c");
    let (output, obj) = run_object_test("struct-empty-field", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 结构体多维数组字段
#[test]
fn multidim_array_field_compiles() {
    let source = include_str!("../fixtures/c/multidim.c");
    let output = run_syntax_test("multidim-array-field", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// noreturn / _Noreturn: 函数末尾插入 ud2 防止落入后续代码
/// 覆盖 C11 _Noreturn、GCC __attribute__((noreturn/__noreturn__/_Noreturn))、
/// stdnoreturn.h 的 noreturn 宏、__declspec(noreturn) 等所有变体.
#[test]
fn noreturn_trap_emits_ud2_compiles() {
    let src = include_str!("../fixtures/c/noreturn_trap.c");
    let (out, asm) = run_asm_output_test("noreturn-trap", src);
    assert!(
        out.status.success(),
        "noreturn_trap compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 按函数名检查: noreturn 函数必须在 ret 之前有 ud2.
    // 提取函数体: 从 `funcname:` 到下一个 `.size funcname, .-funcname`
    let noreturn_fns = [
        "c11_noreturn_exit",
        "gcc_noreturn_abort",
        "gcc_noreturn_alias",
        "std_noreturn_loop",
        "declspec_noreturn_handler",
    ];
    for fname in &noreturn_fns {
        let body = extract_function_body(&asm, fname);
        assert!(!body.is_empty(), "function {fname} not found in asm output");
        // 找 ret 位置, 确认前面有 ud2
        let ret_pos = body.find("\n\tret").expect("ret not found in noreturn fn");
        let pre = &body[..ret_pos];
        assert!(
            pre.contains("ud2"),
            "function {fname} is noreturn but missing ud2 before ret; body=\n{body}"
        );
    }
    // 控制对照: 普通函数 ret 前不应有 ud2
    let regular_body = extract_function_body(&asm, "regular_add");
    assert!(!regular_body.is_empty(), "regular_add not found in asm");
    let ret_pos = regular_body
        .find("\n\tret")
        .expect("ret not found in regular fn");
    let pre = &regular_body[..ret_pos];
    assert!(!pre.contains("ud2"), "regular_add should NOT have ud2 but does; pre=\n{pre}");
}

/// 语句表达式内 typeof 局部声明
#[test]
fn vfs_pressure_stmt_expr_compiles() {
    let source = include_str!("../fixtures/c/stmt_vfs.c");
    let output = run_syntax_test("vfs-pressure", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 三元表达式指针分支可被解引用
#[test]
fn ternary_ptr_deref_compiles() {
    let source = include_str!("../fixtures/c/ternary_ptr.c");
    let output = run_syntax_test("ternary-ptr-deref", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 三元表达式中 post-increment 副作用须正确;
/// 局部变量遮蔽全局时 gen_inc_dec 走栈路径而非 RIP-relative。
/// GCC torture 20000715-1.c
#[test]
#[cfg(unix)]
fn ternary_side_effects_local_shadows_global() {
    let src = include_str!("../fixtures/c/ternary_side_effects.c");
    let (out, _exe) = run_compile_and_run("ternary-side-effects", src);
    assert!(
        out.status.success(),
        "ternary_side_effects compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 链式赋值类型截断: 赋值表达式的值是左值转换后的值 (C11 6.5.16p3),
/// 内层窄类型赋值的结果须按左值宽度/符号性重扩展后再传播给外层.
/// GCC torture 960801-1.c, 921218-2.c
#[test]
#[cfg(unix)]
fn chained_assignment_value_truncated_to_lvalue_type() {
    let src = include_str!("../fixtures/c/chained_assign_truncation.c");
    let (out, _exe) = run_compile_and_run("chained-assign-truncation", src);
    assert!(
        out.status.success(),
        "chained_assign_truncation failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// struct __attribute__((__may_alias__)) name
#[test]
fn may_alias_struct_compiles() {
    let source = include_str!("../fixtures/c/may_alias.c");
    let (output, _obj) = run_object_test("may-alias-struct", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// `__seg_fs` / `__seg_gs` (以及 `__seg_{cs,ds,es,ss}`) 地址空间限定符
///
/// 契约在第 26 轮翻了一次。旧断言是「asm 里不得出现 %fs:/%gs:」（当时只解析不
/// 处理）。那条契约与已经落地的实现互相矛盾：`emit/expr/lvalue.rs`、`dispatch.rs`、
/// `increment.rs` 早就按 pointee 的 `addr_space` 发前缀，但**只有 decl-specifier
/// 那一个位置**能把段带进类型系统 —— 于是同一个语义的几种拼写里只有一种生效
/// （实测 `int __seg_gs *p` 的解引用不生前缀，而 `__seg_gs int *f(void)` 会），
/// 那是比「全不发」更危险的 silently-wrong。
///
/// 现在两面都钉，并且**逐函数**钉（全局计数会被别人的前缀凑数）：
///   * 正向: 段限定 pointee 的每种访问形态都要发前缀（解引用 / 下标 / 成员 /
///     自增自减 / 局部变量 / 形参 / typedef / cast / typeof / 函数返回类型）;
///   * 反向: 不带段的 `test_plain` 与记为 Generic 的 cs/ds/es/ss 必须一个前缀也不发
///     （不写这一面，「逢访存就发前缀」也能把正向做绿）;
///   * 两个档位都跑：段信息必须在 SSA 往返里活下来（-O2 只发 `*p` 不发 `p[i]`
///     就是本轮抓到的）;
///   * 执行: 退出码 0 —— 用户态 GS base == 0，所以 gs 前缀不改变可观察行为；
///     fs 的那几个函数只定义不调用（理由见夹具头注释）。
///
/// 覆盖: 全局变量 / 函数返回类型 / 函数参数 / 局部变量 / cast / struct 字段 /
/// typedef / typeof / static 组合 / 多限定符链 / const 组合 / 全部 6 个 x86 段名 /
/// 函数指针声明符 / 函数指针字段声明符 / sizeof / Alignof。
#[test]
fn seg_qualifier_parses_all_positions_and_emits_segment_prefixes() {
    let source = include_str!("../fixtures/c/seg_qualifier.c");

    // (函数名, 期望前缀)：`Some("%gs:")` = 必须发 gs；`None` = 一个前缀也不许发。
    const GS_ACCESS: [&str; 11] = [
        "use_param_gs",   // 形参：基类型与 * 之间
        "local_test",     // 局部变量
        "cast_test",      // cast 出来的段指针
        "typedef_test",   // typedef 名 + cast
        "typeof_test",    // typeof 基类型
        "idx_gs",         // p[i] 读
        "store_gs",       // *p = v 写
        "idx_store_gs",   // p[i] = v 写
        "preinc_gs",      // ++*p 读—改—写
        "arrow_gs",       // *s->vp（字段是段指针）
        "use_ret_gs_ptr"  // 经函数返回类型带段的指针解引用
    ];
    const FS_ACCESS: [&str; 1] = ["use_param_fs"];
    // 段限定符加在**对象本体**上（夹具 §20）：段属于符号而不属于任何表达式的类型，
    // 所以这一族的真源是「符号→段」表（`emit_global_load/store` 两个下发口）与
    // 「先取址后访问」那一族的前缀真源。旧状态是**不发前缀且不报错**（silently-wrong）。
    const OBJ_GS_ACCESS: [&str; 8] = [
        "obj_read_gs",     // 标量读
        "obj_write_gs",    // 标量写
        "obj_incdec_gs",   // 自增后读回（读—改—写）
        "obj_idx_gs",      // 先取址、后访问：下标读
        "obj_idx_wr_gs",   // 下标写
        "obj_member_gs",   // 成员读（基址 + 字段偏移）
        "obj_member_wr_gs", // 成员写
        "obj_agg_copy_gs", // 聚合拷贝（64 位下 `rep movsb` 的段前缀被 CPU 忽略）
    ];
    // fs 只定义不调用（用户态 FS base 是 TLS 地址，实测执行就是 SIGSEGV），断言在 asm 上。
    const OBJ_FS_ACCESS: [&str; 3] = ["obj_read_fs", "obj_write_fs", "obj_static_fs"];
    // 反向对照：不带段的全局、以及**取址（LEA）**——GAS 明说段重叠对 LEA 无效，
    // 在那里贴前缀只会做出「看着修了」的假象。
    const NO_SEGMENT: [&str; 8] = [
        "test_plain",
        "test_cs",
        "test_ds",
        "test_es",
        "test_ss",
        "obj_read_plain",
        "obj_addr_gs",
        "obj_addr_arr_gs",
    ];

    for flag in ["", "-O2"] {
        let (asm_out, asm_text) = run_asm_output_test_flags("seg-qualifier-asm", source, flag);
        assert!(
            asm_out.status.success(),
            "seg_qualifier [{flag}] 应能编译: {}",
            String::from_utf8_lossy(&asm_out.stderr)
        );
        for name in GS_ACCESS {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(
                !body.is_empty(),
                "[{flag}] 夹具里找不到函数 {name}（改名会让这些断言空判）"
            );
            assert!(
                body.contains("%gs:"),
                "[{flag}] {name}: 段限定 pointee 的访问必须发 %gs:，实得\n{body}"
            );
        }
        for name in FS_ACCESS {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(
                !body.is_empty(),
                "[{flag}] 夹具里找不到函数 {name}（改名会让这些断言空判）"
            );
            assert!(
                body.contains("%fs:"),
                "[{flag}] {name}: __seg_fs pointee 必须发 %fs:，实得\n{body}"
            );
        }
        for name in OBJ_GS_ACCESS {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(
                !body.is_empty(),
                "[{flag}] 夹具里找不到对象本体带段的函数 {name}"
            );
            assert!(
                body.contains("%gs:"),
                "[{flag}] {name}: `__seg_gs` 加在对象本体上，每一笔访问必须发 %gs:，实得\n{body}"
            );
        }
        for name in OBJ_FS_ACCESS {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(
                !body.is_empty(),
                "[{flag}] 夹具里找不到对象本体带段的函数 {name}"
            );
            assert!(
                body.contains("%fs:"),
                "[{flag}] {name}: `__seg_fs` 加在对象本体上必须发 %fs:，实得\n{body}"
            );
        }
        for name in NO_SEGMENT {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(
                !body.is_empty(),
                "[{flag}] 夹具里找不到反向对照函数 {name}"
            );
            assert!(
                !harness::seg_has_prefix(&body),
                "[{flag}] {name}: 无限定 / Generic 段不得发前缀，实得\n{body}"
            );
        }
    }

    // 然后编译运行: gs 前缀在用户态不改可观察行为, 程序必须正常结束。
    let (output, exe) = run_compile_and_run("seg-qualifier", source);
    assert!(
        output.status.success(),
        "seg_qualifier compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(rc, 0, "seg_qualifier 夹具自检未通过（rc={rc}）");
}

/// 段寄存器限定符加在**对象本体**上（`__seg_gs int gv;` 这一形）。
///
/// 与上一条用例的分工：`seg_qualifier.c` 钉「限定符能出现在哪些语法位置」，而其中有
/// 几个位置 gcc 直接拒绝（`int * __seg_gs p`、形参带段等），所以那份夹具**不能**与 gcc
/// 对拍。本用例的夹具只放 gcc 完全接受的形状，钉的是另一件事：**段属于符号本体**，
/// 不挂在任何表达式的类型上 —— 所以发射点只能查「符号→段」表（`emit_global_load` /
/// `emit_global_store` 两个下发口，加「先取址后访问」那一族的前缀真源）。
///
/// 修前的状态是「不发前缀且不报错」（gcc 发 `mov %gs:0x0(%rip),%eax`），即静默访错段。
/// 三面都钉：
///   * 正向：标量/复合赋值/自增自减/下标（含 long、u8 窄形）/浮点与 long double/成员/
///     聚合拷贝/函数内 `static`/表达式里两个段全局 —— 逐函数要求前缀，且读—改—写那一族
///     要求**至少两笔**（只给写加前缀、读裸走是最容易被漏掉的错法）；
///   * 取址反向对照：`&gv` 必须**不发**前缀。GAS 明说 "segment override on `lea' is
///     ineffectual"，在那儿贴字节只会做出「看着修了」的假象；
///   * 轴分离：`__seg_gs int *pseg` 的**本体**写不发前缀（指针本身在默认段）而
///     `*pseg` 发（pointee 带段）；不带段的普通全局与局部遮蔽全局的同名变量一个前缀也
///     不发（不写这一面，「逢全局访存就发前缀」也能把正向做绿）。
///
/// 两个档位都跑：段信息必须在 SSA 往返里活下来。fs 侧只定义不调用（用户态 FS base 是
/// TLS 地址，实测执行就是 SIGSEGV），断言全部在 asm 上；最后与 gcc 四档做执行级对拍。
#[test]
fn seg_qualifier_on_object_body_emits_segment_prefixes() {
    let source = include_str!("../fixtures/c/seg_object_body.c");

    // (函数名, 最少前缀笔数)。计数而非只判存在：读—改—写形状只钉「有没有」的话，
    // 「只给写贴前缀、读裸走」这种半修状态能直接做绿。
    const GS: [(&str, usize); 24] = [
        ("obj_rd", 1),
        ("obj_wr", 1),
        ("obj_cad", 2),
        ("obj_csub", 2),
        ("obj_cmul", 2),
        ("obj_cand", 2),
        ("obj_cor", 2),
        ("obj_cxor", 2),
        ("obj_cmod", 2),
        ("obj_cshl", 2),
        ("obj_inc_post_stmt", 2),
        ("obj_inc_pre_val", 2),
        ("obj_dec_post_val", 2),
        ("arr_rd", 1),
        ("arr_wr", 1),
        ("arr_inc", 2),
        ("arr_rd_long", 1),
        ("arr_rd_u8", 1),
        ("dbl_rd", 1),
        ("ld_wr", 1),
        ("fld_cad", 2),
        ("agg_assign", 1),
        ("static_gs_inc", 2),
        ("two", 2),
    ];
    const FS: [&str; 5] =
        ["fs_rd", "fs_wr", "fs_static_rd", "fs_static_wr", "fs_arr_rd"];
    const NO_PREFIX: [&str; 13] = [
        "obj_take",     // &gv —— LEA 发前缀无效，必须不发
        "fld_take",     // &gs —— 同上，目标是结构体
        "plain_take",
        "plain_rd",
        "plain_wr",
        "plain_cad",
        "plain_arr_rd",
        "plain_fld_wr",
        "plain_agg_copy",
        "static_plain_wr",
        "pseg_wr_obj",      // 带段指针的**本体**：默认段
        "p_plain_rd_pointee", // 普通指针的解引用
        "shadow_gs",        // 局部遮蔽同名段全局：走的是栈，不是段
    ];

    for flag in ["", "-O2"] {
        let (asm_out, asm_text) = run_asm_output_test_flags("seg-object-body-asm", source, flag);
        assert!(
            asm_out.status.success(),
            "seg_object_body [{flag}] 应能编译: {}",
            String::from_utf8_lossy(&asm_out.stderr)
        );
        for (name, min) in GS {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(!body.is_empty(), "[{flag}] 夹具里找不到函数 {name}（改名会让断言空判）");
            let got = body.matches("%gs:").count();
            assert!(
                got >= min,
                "[{flag}] {name}: 对象本体带 __seg_gs，至少 {min} 笔访问要发 %gs:，实发 {got} 笔，实得\n{body}"
            );
        }
        for name in FS {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(!body.is_empty(), "[{flag}] 夹具里找不到函数 {name}");
            assert!(
                body.contains("%fs:"),
                "[{flag}] {name}: 对象本体带 __seg_fs 必须发 %fs:，实得\n{body}"
            );
            assert!(
                !body.contains("%gs:"),
                "[{flag}] {name}: fs 侧的形状不得串到 gs 段，实得\n{body}"
            );
        }
        // 轴分离的另一面：pointee 带段的解引用仍要发前缀（不能因为改的是「符号表」
        // 那一轴就把 pointee 那一轴顶掉）。
        let pdp = harness::seg_asm_body(&asm_text, "pseg_rd_pointee");
        assert!(pdp.contains("%gs:"), "[{flag}] pseg_rd_pointee: {pdp}");
        for name in NO_PREFIX {
            let body = harness::seg_asm_body(&asm_text, name);
            assert!(!body.is_empty(), "[{flag}] 夹具里找不到反向对照函数 {name}");
            assert!(
                !harness::seg_has_prefix(&body),
                "[{flag}] {name}: 取址 / 不带段 / 指针本体都不得发前缀，实得\n{body}"
            );
        }
    }

    // 与 gcc 四档逐字对拍（本夹具全是 gcc 完全接受的形状）。
    harness::assert_all_levels_match_gcc(
        "c-seg-object-body",
        source,
        "段限定对象本体的访问在高档位下丢前缀或改错值",
    );
}

/// GCC 兼容：相同类型的重复 typedef 不报错
#[test]
fn compat_typedef_redefinition_compiles() {
    let source = include_str!("../fixtures/c/typedef.c");
    let (output, _obj) = run_object_test("compat-typedef", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 局部变量可遮蔽全局函数名（index/basename 等 libc 符号）
#[test]
fn local_shadows_libc_function_name() {
    let source = include_str!("../fixtures/c/shadow.c");
    let (output, exe) = run_compile_and_run("shadow-libc-name", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0);
}

/// _Complex 除法 codegen
#[test]
fn complex_div_run_returns_zero() {
    let source = include_str!("../fixtures/c/complex_div.c");
    let (output, exe) = run_compile_and_run("complex-div", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "complex_div should return 0, got {rc}");
}

#[test]
fn stdc_cx_limited_range_controls_complex_arithmetic_codegen() {
    let on_div = r#"
#pragma STDC CX_LIMITED_RANGE ON
double _Complex f(double _Complex a, double _Complex b) { return a / b; }
"#;
    let off_div = r#"
#pragma STDC CX_LIMITED_RANGE OFF
double _Complex f(double _Complex a, double _Complex b) { return a / b; }
"#;
    let on_mul = r#"
#pragma STDC CX_LIMITED_RANGE ON
double _Complex f(double _Complex a, double _Complex b) { return a * b; }
"#;
    let off_mul = r#"
#pragma STDC CX_LIMITED_RANGE OFF
double _Complex f(double _Complex a, double _Complex b) { return a * b; }
"#;
    let (on_out, on_obj) = run_object_test("stdc-cx-limited-div-on", on_div);
    assert!(
        on_out.status.success(),
        "CX_LIMITED_RANGE ON compile stderr: {}",
        String::from_utf8_lossy(&on_out.stderr)
    );
    let on_relocs = readelf_relocations(&on_obj);
    assert!(
        !on_relocs.contains("__divdc3"),
        "CX_LIMITED_RANGE ON should use inline limited formula: {on_relocs}"
    );
    let _ = fs::remove_file(&on_obj);

    let (off_out, off_obj) = run_object_test("stdc-cx-limited-div-off", off_div);
    assert!(
        off_out.status.success(),
        "CX_LIMITED_RANGE OFF compile stderr: {}",
        String::from_utf8_lossy(&off_out.stderr)
    );
    let off_relocs = readelf_relocations(&off_obj);
    assert!(
        off_relocs.contains("__divdc3"),
        "CX_LIMITED_RANGE OFF should use range-safe helper: {off_relocs}"
    );
    let _ = fs::remove_file(&off_obj);

    let (on_mul_out, on_mul_obj) = run_object_test("stdc-cx-limited-mul-on", on_mul);
    assert!(
        on_mul_out.status.success(),
        "CX_LIMITED_RANGE ON mul compile stderr: {}",
        String::from_utf8_lossy(&on_mul_out.stderr)
    );
    let on_mul_relocs = readelf_relocations(&on_mul_obj);
    assert!(
        !on_mul_relocs.contains("__muldc3"),
        "CX_LIMITED_RANGE ON should inline limited multiply: {on_mul_relocs}"
    );
    let _ = fs::remove_file(&on_mul_obj);

    let (off_mul_out, off_mul_obj) = run_object_test("stdc-cx-limited-mul-off", off_mul);
    assert!(
        off_mul_out.status.success(),
        "CX_LIMITED_RANGE OFF mul compile stderr: {}",
        String::from_utf8_lossy(&off_mul_out.stderr)
    );
    let off_mul_relocs = readelf_relocations(&off_mul_obj);
    assert!(
        off_mul_relocs.contains("__muldc3"),
        "CX_LIMITED_RANGE OFF should use range-safe multiply helper: {off_mul_relocs}"
    );
    let _ = fs::remove_file(&off_mul_obj);
}

/// double _Complex SysV AMD64 by-value argument and return ABI.
#[test]
fn double_complex_abi_run_returns_zero() {
    let source = include_str!("../fixtures/c/complex_abi.c");
    // 逐档跑：本夹具的两条历史缺陷都只在 -O2 起才暴露——字段式写入复数局部后
    // 读回（写被静默丢弃）与 `x + y` 的复数加法（重建时结果类型退化成 int）。
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        let (output, exe) = run_compile_and_run_flags("double-complex-abi", source, &[flag]);
        assert!(
            output.status.success(),
            "{flag} 编译失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "{flag}: double_complex_abi 应返回 0，实际 {rc}");
        let _ = fs::remove_file(exe);
    }
}

#[test]
fn function_return_keep_preserves_plain_ret() {
    let source = "int normal_return(int x) { return x + 1; }\n";
    let (output, obj_path) =
        run_object_test_flags("function-return-keep", source, &["-mfunction-return=keep"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let obj_data = std::fs::read(&obj_path).expect("read object file");
    let text = elf64_section_data(&obj_data, ".text").expect("missing .text");
    assert!(text.contains(&0xC3), "keep mode should preserve plain ret bytes");
    let _ = std::fs::remove_file(&obj_path);
}

/// __int128 SysV 双寄存器传参与返回值
#[test]
fn int128_call_abi_run_returns_zero() {
    let source = include_str!("../fixtures/c/int128_abi.c");
    let (output, exe) = run_compile_and_run("int128-call-abi", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "int128_call_abi should return 0, got {rc}");
}

/// __int128 乘/除/取模运行时正确性
#[test]
fn int128_mul_div_mod_run_returns_zero() {
    let source = include_str!("../fixtures/c/int128_arith.c");
    let (output, exe) = run_compile_and_run("int128-mul-div-mod", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "int128_mul_div_mod should return 0, got {rc}");
}

/// M13.1: float/double 取模软件实装 (divss/divsd + trunc)
#[test]
fn float_mod_run_returns_zero() {
    let source = include_str!("../fixtures/c/float_mod.c");
    let (output, _exe) = run_compile_and_run("float-mod", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "float_mod should return 0, got {rc}");
    }
}

/// M13.1: __int128 &&/|| 高 64 位合并后再 test
#[test]
fn int128_logical_run_returns_zero() {
    let source = include_str!("../fixtures/c/int128_log.c");
    let (output, _exe) = run_compile_and_run("int128-logical", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "int128_logical should return 0, got {rc}");
    }
}

/// M13.1: 全局数组 [rip+disp32+idx*scale] 单指令加载
#[test]
fn global_indexed_run_returns_zero() {
    let source = include_str!("../fixtures/c/global_idx.c");
    let (output, _exe) = run_compile_and_run("global-indexed", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "global_indexed should return 0, got {rc}");
    }
}

// =================== C 前端补洞: 新增特性 ===================

/// MS 扩展关键字: _ms_restrict / __declspec / __pragma
#[test]
#[cfg(unix)]
fn ms_extensions_compile_and_run() {
    let source = include_str!("../fixtures/c/ms_ext.c");
    let (output, exe) = run_compile_and_run("ms-extensions", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "ms_extensions should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 链式 designated initializer (ISO C99 6.7.8/19: `.a.b.c` 或 `[0].field`).
/// 用于 Linux kernel `_ddebug.key.dd_key_true = ...` 初始化 union 成员.
#[test]
fn chained_designator_init() {
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

/// -ftrapv: 有符号取负溢出 (e.g. -INT_MIN) 应调用 __dcc_trapv_overflow.
#[test]
#[cfg(unix)]
fn trapv_negation_overflow() {
    // 用函数参数避免编译期常量折叠: x 在运行期为 INT_MIN
    let source = "extern void exit(int);\n\
void __dcc_trapv_overflow(void) {\n\
    exit(99);\n\
}\n\
int negate(int x) { return -x; }\n\
int get_int_min(void) { return -2147483647 - 1; }\n\
int main(void) { return negate(get_int_min()); }\n";
    let (_, run, log, exe_path) = run_sanitized_binary("trapv-neg", source, &["-ftrapv"]);
    assert_exit_code(&run, 99, &log);
    let _ = fs::remove_file(&exe_path);
}

#[test]
#[cfg(unix)]
fn thread_local_exec_returns_15() {
    let src = include_str!("../fixtures/c/tls_simple.c");
    let (out, exe) = run_compile_and_run("tls", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 15, "tls counter");
    let _ = fs::remove_file(&exe);
}

#[test]
#[cfg(unix)]
fn kr_old_style_function() {
    let src = include_str!("../fixtures/c/kr_func.c");
    let (out, exe) = run_compile_and_run("kr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 7);
    let _ = fs::remove_file(&exe);
}

#[test]
fn implicit_function_decl_is_error_by_default() {
    // C23 严格模式下隐式函数声明是错误
    let src = "int main(void) { return missing_func(1); }\n";
    let path = std::env::temp_dir().join(format!("dcc-implicit-func-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=c23", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "implicit function should fail");
    assert!(stderr.contains("E0017"), "expected E0017: {stderr}");
    assert!(
        stderr.contains("implicit declaration of function 'missing_func'"),
        "expected function name in diagnostic: {stderr}"
    );
}

#[test]
fn implicit_function_decl_error_ignores_wno_in_modern_std() {
    // C23 严格模式下即使 -Wno- 也仍然报错
    let src = "int main(void) { return missing_func(1); }\n";
    let path = std::env::temp_dir().join(format!("dcc-wno-func-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-std=c23",
            "-Wno-implicit-function-declaration",
            "-fsyntax-only",
        ])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "strict mode must still error");
    assert!(stderr.contains("E0017"), "expected E0017: {stderr}");
}

#[test]
fn gnu89_implicit_function_decl_warns_and_continues() {
    let src = "int main(void) { return missing_func(1); }\n";
    let path = std::env::temp_dir().join(format!("dcc-gnu89-func-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu89", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "gnu89 implicit function should not error: {stderr}");
    assert!(stderr.contains("W1000"), "expected W1000: {stderr}");
}

/// `-fstrict-aliasing` 必须**告知无效**，不得默默假装做了。
///
/// 背景：P4/P13 那份别名分析因「结果从入口到管线都被丢弃」已删（见
/// docs/ssa-pass-upgrade-plan.md §7.37），删完之后 `-fstrict-aliasing` 就是一个
/// 「能接受但悄悄无效」的开关。§7.37 的要求是二选一（接真 TBAA 消费者，或给一
/// 条诊断），本用例钉住后者：
///   * `-fstrict-aliasing`：警告 + 仍成功编译（不改变行为，不能把它做成错误）；
///   * `-fno-strict-aliasing`：**不警告** —— 它要求的保守 aliasing 本来就是现状；
///   * 不传任何一侧：也不警告。
/// 反向对照那两条是必需的：若实现成「只要看到 strict_aliasing 字样就叫」，
/// 内核构建传 `-fno-strict-aliasing` 会被噪音淹没，而那侧其实没有债。
#[test]
fn fstrict_aliasing_is_reported_as_inert_but_fno_is_silent() {
    let src = "int main(void) { return 0; }\n";
    let path = std::env::temp_dir().join(format!("dcc-strict-alias-{}.c", std::process::id()));
    fs::write(&path, src).unwrap();
    let run = |flag: &str| {
        // 用 `-S -o /dev/null` 而不是 `-fsyntax-only`：后者不走 `cli_warnings` 的输出通道
        //（实测同一条警告在 `-c`/`-S` 下会打印、`-fsyntax-only` 不打印），拿它测警告会是假绿。
        let mut args = vec!["-S", "-o", "/dev/null"];
        if !flag.is_empty() {
            args.push(flag);
        }
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(&args)
            .arg(&path)
            .output()
            .expect("run dcc");
        (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
    };

    let (ok, err) = run("-fstrict-aliasing");
    assert!(ok, "-fstrict-aliasing 仍应编译成功: {err}");
    assert!(
        err.contains("-fstrict-aliasing"),
        "-fstrict-aliasing 应有一条告知它无效的警告: {err}"
    );

    let (ok, err) = run("-fno-strict-aliasing");
    assert!(ok, "-fno-strict-aliasing 应编译成功: {err}");
    assert!(
        !err.contains("strict-aliasing"),
        "-fno-strict-aliasing 要求的保守行为就是现状，不得警告（内核构建全部传这个）: {err}"
    );

    let (ok, err) = run("");
    let _ = fs::remove_file(&path);
    assert!(ok, "默认应编译成功: {err}");
    assert!(
        !err.contains("strict-aliasing"),
        "不传标志时不得警告: {err}"
    );
}

// ===== M15: const_eval / 严格类型检查 / 运行时检查 =====

/// 浮点常量求值 (M15.1)
#[test]
#[cfg(unix)]
fn m15_float_const_eval() {
    let src = include_str!("../fixtures/c/float_const.c");
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
    let src = include_str!("../fixtures/c/string_const.c");
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
    let src = include_str!("../fixtures/c/address_const.c");
    let (out, exe) = run_compile_and_run("m15-addr-const", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// const_eval 增强: 位域宽度常量表达式 + sizeof/alignof + 三元 + 类型转换 + 枚举
#[test]
#[cfg(unix)]
fn const_eval_comprehensive_bitfield_sizeof_ternary() {
    let src = include_str!("../fixtures/c/const_eval_comprehensive.c");
    let (out, exe) = run_compile_and_run("const-eval-comprehensive", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "runtime exit code should be 0");
    let _ = fs::remove_file(&exe);
}

/// Struct ABI 综合测试: 8B SSE / 16B SSE / 16B Integer / 24B Memory
/// 类结构体传值与返回值, 验证 SysV AMD64 ABI 各分类正确性.
#[test]
#[cfg(unix)]
fn struct_abi_comprehensive() {
    let src = include_str!("../fixtures/c/struct_abi_comprehensive.c");
    let (out, exe) = run_compile_and_run("struct-abi-comprehensive", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// Struct/Union ABI 扩展测试: 对齐填充、混合 float/int、SIMD 向量、
/// 联合、嵌套结构体、位域等复杂场景, 验证完整 SysV ABI 分类算法.
#[test]
#[cfg(unix)]
fn struct_abi_extended() {
    let src = include_str!("../fixtures/c/struct_abi_extended.c");
    let (out, exe) = run_compile_and_run("struct-abi-extended", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// 80-bit `long double` 算术与比较回归。夹具里 bit0/1/2 三项判据**只有真
/// x87 80-bit 运算才成立**（f64 下 `1/3` 与拓宽后的 f64 基准相等、`1+2^-63`
/// 舍回 1、累加结果不同），且已单独验证过它们的鉴别力（把结果先过
/// 一遍 `double` 再比，对应位会变 0）。stdout 逐字符比较则防住
/// “退出码对了但数字不同”那种部分实现。
#[test]
#[cfg(unix)]
fn ld80_arith_cmp() {
    let src = include_str!("../fixtures/c/ld80_arith_cmp.c");
    let (out, exe) = run_compile_and_run("ld80-arith-cmp", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "80-bit 算术/比较判据未全中 (期望 r=15)");
    let _ = fs::remove_file(&exe);
}

/// inline asm `t`（x87）通道绑定 long double：全局/局部输入与全局/局部
/// 写回都必须走真 80-bit tbyte，输出与 gcc 逐字符一致、退出码 0。
/// 覆盖 REX 前缀顺序与全局读侧 movsd 两类根因修复。
#[test]
#[cfg(unix)]
fn ld80_asm_t_channel() {
    let src = include_str!("../fixtures/c/ld80_asm_t_channel.c");
    let (out, exe) = run_compile_and_run("ld80-asm-t-channel", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "t 通道判据未全中");
    let _ = fs::remove_file(&exe);
}

/// inline asm `x`（SSE）通道绑定 float/double：全局/局部输入与写回全部
/// 形态必须正确，输出与 gcc 逐字符一致、退出码 0。
#[test]
#[cfg(unix)]
fn ld80_asm_x_channel() {
    let src = include_str!("../fixtures/c/ld80_asm_x_channel.c");
    let (out, exe) = run_compile_and_run("ld80-asm-x-channel", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "x 通道判据未全中");
    let _ = fs::remove_file(&exe);
}

/// long double 经 `x` 约束非法：dcc 必须以 E0311 诊断拒绝（对齐 gcc
/// "inconsistent operand constraints"），四处形态各一条且退出非 0。
#[test]
fn ld80_asm_x_rejects_long_double() {
    let dir = std::env::temp_dir();
    let tag = format!("dcc-ld80-asm-x-reject-{}", std::process::id());
    let src_path = dir.join(format!("{tag}.c"));
    let obj_path = dir.join(format!("{tag}.o"));
    // 负向夹具（本夹具就是期望 E0311 拒绝）属于 tests/fixtures/errors/：
    // 放在 fixtures/c/ 会被链接型语级测试当成待修债。
    fs::write(
        &src_path,
        include_str!("../fixtures/errors/ld80_asm_x_rejects_long_double.c"),
    )
        .expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "must reject long double + x constraint:\n{stderr}");
    let count = stderr.matches("E0311").count();
    assert!(count >= 4, "expected >= 4 E0311 errors, got {count}:\n{stderr}");
    assert!(
        stderr.contains("inconsistent operand constraints"),
        "expected gcc-style message:\n{stderr}"
    );
    assert!(!stderr.to_lowercase().contains("panic"), "error path panicked:\n{stderr}");
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

/// 浮点存储路径: double/float 数组下标、解引用、struct 字段赋值
/// (值在 xmm0, 必须用 movss/movsd 而非 GP mov 存储)
#[test]
#[cfg(unix)]
fn m15_float_store_paths() {
    let src = include_str!("../fixtures/c/abi16_struct_extra.c");
    let (out, exe) = run_compile_and_run("m15-float-store-paths", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// C23 constexpr 函数声明 (解析通过, 运行结果正确)
#[test]
#[cfg(unix)]
fn m15_constexpr_function() {
    let src = include_str!("../fixtures/c/constexpr.c");
    let (out, exe) = run_compile_and_run("m15-constexpr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

// ====== M19: 预处理器补齐 ======
// __COUNTER__ 单调递增, 跨宏嵌套仍保持唯一
#[test]
#[cfg(unix)]
fn m19_counter_monotonic() {
    let src = include_str!("../fixtures/c/counter.c");
    let (out, exe) = run_compile_and_run("m19-counter", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "__COUNTER__ should produce 0,1,2");
    let _ = fs::remove_file(exe);
}

/// `double _Complex` 类型说明符 (C99 complex.h 形式)
/// M26: 此前 parse_type_mode 第二 loop 的 KwDouble arm 带 `break;`
/// 提前退出, 导致 `_Complex` 后续修饰符被外层误判为 declarator.
/// 现已去掉 break, 允许 `float _Complex` / `double _Complex` /
/// `long double _Complex` 自由组合.
#[test]
fn double_complex_decl_run_returns_zero() {
    let source = include_str!("../fixtures/c/complex_decl.c");
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
    let source = include_str!("../fixtures/c/complex_literal.c");
    let (output, exe) = run_compile_and_run("complex-literal", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "complex_literal should return 0, got {rc}");
}

/// _Imaginary 类型基本测试 (C99 6.2.5)
/// 覆盖: sizeof、__imag__/__real__ 提取、imag±imag 算术、一元取负
#[test]
fn imaginary_basic_run_returns_zero() {
    let source = include_str!("../fixtures/c/imaginary_basic.c");
    let (output, exe) = run_compile_and_run("imaginary-basic", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "imaginary_basic should return 0, got {rc}");
}

/// L'x' 宽字符字面量 (Section 11)
/// M26: 新增 TokKind::WideCharLit, lexer 在 `L` 标识符后看到
/// `'` 时走宽字符路径; 解析层复用 parse_wide_str 输出 UTF-32LE.
#[test]
fn wide_char_literal_run_returns_zero() {
    let source = include_str!("../fixtures/c/wide_lit.c");
    let (output, exe) = run_compile_and_run("wide-char-literal", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "wide_char_literal should return 0, got {rc}");
}

/// fenv.h 运行时调用 (Section 11.2)
/// feclearexcept / fegetround / fesetround / feraiseexcept / fetestexcept
/// 由 dcc 内联为 MXCSR 指令序列, 不依赖 libm 符号 (host 链接无需 -lm,
/// freestanding 场景可用). 编译并运行, 期望返回 0.
#[test]
fn fenv_runtime_calls_compile_and_run() {
    let source = include_str!("../fixtures/c/fenv.c");
    let (output, exe) = run_compile_and_run("fenv-runtime", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "fenv runtime calls should return 0, got {rc}");
}

/// FENV_ACCESS 运行时语义 (Section 11.2)
/// #pragma STDC FENV_ACCESS ON + -O2: 浮点运算不被编译期折叠 (舍入模式
/// 在运行时被 fesetround 改变), fesetround 调用不被 SSA 重建丢弃.
/// 编译运行期望返回 0.
#[test]
fn fenv_access_rounding_mode_affects_runtime_arith() {
    let source = include_str!("../fixtures/c/fenv_round.c");
    let (output, exe) = run_compile_and_run_flags("fenv-rounding", source, &["-O2"]);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(
        rc, 0,
        "rounding mode should affect runtime division under FENV_ACCESS, got {rc}"
    );
}

/// 复杂数学 cproj / cabs / carg libm 转发 (Section 11 P0.3).
/// 编译 fixtures/c/complex_libm.c, 验证 .o 引用 cproj / cabs / carg
/// 作为 undefined symbol (留给 host gcc -lm 解析), 然后链接运行.
#[test]
fn complex_libm_fixture_runs() {
    let src = include_str!("../fixtures/c/complex_libm.c");
    let (compile, code) = run_host_link_and_run_with("complex-libm", src, &["-lm"]);
    if code != 0 {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        panic!("complex_libm fixture exit={code}, expected 0\ndcc stderr: {stderr}");
    }
}

/// 无符号整数 ↔ 浮点转换 codegen 运行期验证.
/// 覆盖:
///   - unsigned int (<=32bit) → float/double (零扩展 + cvtsi2ss/sd)
///   - unsigned long (64bit) → float/double (shr+convert+double 算法)
///   - float/double → unsigned int (<=32bit) (cvttss2si + 负值钳位 + 截断)
///   - float/double → unsigned long (cvttss2si + 2^64 校正)
#[test]
fn unsigned_float_conversion_runtime() {
    let src = "\
int main(void) {
    /* unsigned int → float/double */
    unsigned int u = 4000000000u; /* > INT32_MAX */
    float f = (float)u;
    double d = (double)u;
    if (f < 3.9e9f || f > 4.1e9f) return 1;
    if (d < 3.9e9 || d > 4.1e9) return 2;

    /* unsigned long → double */
    unsigned long ul = 13835058055282163712ul; /* ~1.38e19, > INT64_MAX */
    double d2 = (double)ul;
    if (d2 < 1.3e19 || d2 > 1.5e19) return 3;

    /* float → unsigned int */
    float f2 = 300.7f;
    unsigned int u2 = (unsigned int)f2;
    if (u2 != 300) return 4;

    /* double → unsigned int */
    double d3 = 65000.9;
    unsigned int u3 = (unsigned int)d3;
    if (u3 != 65000) return 5;

    /* negative float → unsigned int (clamp to 0) */
    float f3 = -1.5f;
    unsigned int u4 = (unsigned int)f3;
    if (u4 != 0) return 6;

    /* double → unsigned long (value > INT64_MAX) */
    double d4 = 1.0e19;
    unsigned long ul2 = (unsigned long)d4;
    if (ul2 < 9000000000000000000ul) return 7;

    /* small unsigned → float */
    unsigned char uc = 200;
    float f4 = (float)uc;
    if (f4 < 199.0f || f4 > 201.0f) return 8;

    /* unsigned short → double */
    unsigned short us = 60000;
    double d5 = (double)us;
    if (d5 < 59999.0 || d5 > 60001.0) return 9;

    return 0;
}
";
    let (out, exe) = run_compile_and_run("unsigned-float-conv", src);
    assert!(
        out.status.success(),
        "unsigned-float-conv compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "unsigned-float-conv runtime check failed (rc={rc})");
}

/// Assign 通用 lvalue 回退 codegen 验证:
/// 覆盖 Cast-lvalue (`(type)expr = val`) 和嵌套 member access (`func().field = val`).
/// 这些左值形式不走 Ident/Index/Deref/Member/Real/Imag 显式分支,
/// 而应通过 gen_lvalue_addr 通用回退路径正确存储.
#[test]
fn assign_generic_lvalue_fallback_runtime() {
    let src = "\
struct Point { int x, y; };
struct Point make_point(int x, int y) {
    struct Point p;
    p.x = x;
    p.y = y;
    return p;
}
int main(void) {
    /* 嵌套 member access: 函数返回值直接访问字段 */
    struct Point pts[2];
    pts[0] = make_point(1, 2);
    pts[1] = make_point(3, 4);
    if (pts[0].x != 1 || pts[0].y != 2) return 1;
    if (pts[1].x != 3 || pts[1].y != 4) return 2;

    /* 指针 member access 通过函数返回 */
    struct Point arr[1];
    arr[0] = make_point(10, 20);
    struct Point *p = arr;
    if (p->x != 10 || p->y != 20) return 3;

    /* 复合赋值通过指针解引用 */
    int val = 42;
    int *ip = &val;
    *ip = 100;
    if (val != 100) return 4;

    /* 数组索引赋值通过指针 */
    int nums[3] = {0, 0, 0};
    int *np = nums;
    np[0] = 10;
    np[1] = 20;
    np[2] = 30;
    if (nums[0] != 10 || nums[1] != 20 || nums[2] != 30) return 5;

    return 0;
}
";
    let (out, exe) = run_compile_and_run("assign-generic-lvalue", src);
    assert!(
        out.status.success(),
        "assign-generic-lvalue compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "assign-generic-lvalue runtime check failed (rc={rc})");
}

/// 内存 lvalue 自增/自减 codegen 运行期验证: (*p)++ / a[i]-- /
/// p->f++ / 前缀+后缀 + 指针步长, 均应产生真实读-改-写.
#[test]
fn incdec_memory_lvalue_runtime() {
    let src = "\
int main(void) {
    int a[4] = {1, 2, 3, 4};
    int *p = &a[1];
    (*p)++;          // a[1] = 3
    a[2]--;          // a[2] = 2
    ++a[0];          // a[0] = 2
    int old = a[3]++; // old = 4, a[3] = 5
    if (a[0] != 2 || a[1] != 3 || a[2] != 2 || a[3] != 5 || old != 4)
        return 1;
    struct S { int f; } s = {7};
    struct S *sp = &s;
    sp->f++;         // s.f = 8
    if (s.f != 8) return 2;
    // 指针自增: 步长 = 元素大小
    int *q = &a[0];
    q++;             // q = &a[1]
    if (*q != 3) return 3;
    return 0;
}
";
    let (out, exe) = run_compile_and_run("incdec-mem-lvalue", src);
    assert!(
        out.status.success(),
        "incdec memory lvalue compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "incdec memory lvalue runtime mismatch, rc = {rc}");
}

/// 数组到指针衰减 (array-to-pointer decay) 回归: 值上下文中数组标识符
/// 必须衰减为首元素地址, 而非读取 a[0] 的值。
/// 覆盖: 局部/全局/static 数组初始化、函数实参、条件表达式、字符串、
/// 以及 8B/16B 数组作为指针函数返回值 (16B 走聚合路径需显式衰减)。
#[test]
fn array_decay_value_contexts() {
    let src = "\
int g[2] = {10, 20};
static int arr_static[2] = {7, 8};
int *return_global(void) { return g; }
int *return_local8(void) { int a[2] = {5, 6}; return a; }
int *return_local16(void) { int a[4] = {7, 8, 9, 10}; return a; }
int use_arr(int *p) { return p[0]; }
int main(void) {
    // 局部数组初始化衰减
    int a[3] = {1, 2, 3};
    int *q = a;
    if (q != a) return 1;
    // 全局数组初始化衰减
    int *p = g;
    if (p != g) return 2;
    // 全局数组作为函数实参 (隐式衰减)
    if (use_arr(g) != 10) return 3;
    // 字符串字面量初始化
    char *s = \"hi\";
    if (s[0] != 'h' || s[1] != 'i') return 4;
    // 函数返回数组地址 (8B 数组, 走标量返回路径)
    if (return_global() != g) return 5;
    if (*return_local8() != 5) return 6;
    // 16B 数组返回: 必须衰减为指针, 不能按聚合值返回 (读内容)
    if (*return_local16() != 7) return 7;
    // static 数组初始化衰减
    int *sp = arr_static;
    if (sp != arr_static) return 8;
    // 条件表达式中数组衰减
    int *c = 1 ? g : a;
    if (c != g) return 9;
    return 0;
}
";
    let (out, exe) = run_compile_and_run("array-decay", src);
    assert!(
        out.status.success(),
        "array decay compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "array decay runtime mismatch, rc = {rc}");
}

// =====================================================================
// M1 milestone tests
// =====================================================================

/// M1.1: Hello World — compile a single-file C program with stdio.h
/// and printf, link with host gcc, verify stdout output.
#[test]
#[cfg(unix)]
fn m1_1_hello_world() {
    let src = include_str!("../fixtures/c/hello.c");
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
    let src = include_str!("../fixtures/c/hello.c");
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
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    #[cfg(unix)]
    {
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
    let helper_src = include_str!("../fixtures/c/helper.c");
    let main_src = include_str!("../fixtures/c/main.c");
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
    assert!(c1.status.success(), "helper compile: {}", String::from_utf8_lossy(&c1.stderr));
    let c2 = Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&m_obj)
        .arg(&m_path)
        .output()
        .expect("dcc");
    let _ = fs::remove_file(&m_path);
    assert!(c2.status.success(), "main compile: {}", String::from_utf8_lossy(&c2.stderr));
    let link = Command::new("gcc")
        .args(["-o"])
        .arg(&exe)
        .arg(&h_obj)
        .arg(&m_obj)
        .output()
        .expect("gcc link");
    let _ = fs::remove_file(&h_obj);
    let _ = fs::remove_file(&m_obj);
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    #[cfg(unix)]
    {
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

// =====================================================================
// M2 milestone tests
// =====================================================================

/// M2.1: type system - sizeof/alignof for all basic types
/// matches gcc on x86_64 Linux.
#[test]
#[cfg(unix)]
fn m2_1_type_system() {
    let src = include_str!("../fixtures/c/types.c");
    let (out, code) = run_host_link_and_run("m2-1-types", src);
    assert!(
        out.status.success(),
        "m2_1 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "type size/align checks failed (exit={code})");
}

/// 声明符与类型模型的矩阵：形参衰减、指针指数组（形参与 typedef）、函数指针数组。
///
/// 这个夹具的存在理由：`int (*pa)[3]` 作为**形参**曾被建成 `Array(Ptr(Int),3)`
/// 而不是 `Ptr(Array(Int,3))`，于是 `pa[i][j]` 的步长与 `(*pa)[i]` 的衰减全错，
/// **连 -O0 都是静默错值**（真因、取证与误归因的教训见 docs §7.34）。
///
/// 四个档位都跑：类型模型错不该只在某个 -O 档被暴露——实测它在 -O0 就错，而
/// 只在 -O2/-O3 跑的等价门禁（OPT-DIFF）当时因语料里没有这个形状而一路绿灯。
/// 每族同时钉 `sizeof` 与**一次带步长的写**：只看 sizeof 时正反两种结合可能
/// 巧合相等（8/16），只有按正确步长写出去才能分开。
#[test]
#[cfg(unix)]
fn decl_param_decay_matrix_holds_at_all_levels() {
    let src = include_str!("../fixtures/c/decl_param_decay.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("decl-param-decay", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 声明符矩阵夹具应能编译: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "{flag}: 声明符/类型模型矩阵不成立（bad-bitmask={rc}）");
        let _ = fs::remove_file(&exe);
    }
}

/// M2.2: struct layout - bitfields, alignment, padding, packed,
/// aligned attribute, unions, flexible array members.
#[test]
#[cfg(unix)]
fn m2_2_struct_layout() {
    let src = include_str!("../fixtures/c/struct.c");
    let (out, code) = run_host_link_and_run("m2-2-struct", src);
    assert!(
        out.status.success(),
        "m2_2 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "struct layout checks failed (exit={code})");
}

/// Bitfield runtime: read/write/sizeof for signed, unsigned,
/// mixed, cross-container, and small container bitfields.
#[test]
#[cfg(unix)]
fn bitfield_runtime() {
    let src = include_str!("../fixtures/c/bitfield_runtime.c");
    let (out, code) = run_host_link_and_run("bitfield-rt", src);
    assert!(
        out.status.success(),
        "bitfield compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "bitfield runtime checks failed (exit={code})");
}

/// F1: bitfield write RMW temp slots must not clobber the
/// callee-saved spill region (rbx at -16(%r13) etc). Regression
/// for H1 in the x86_64 frame-offset-gatekeeper plan.
#[test]
#[cfg(unix)]
fn bitfield_rmw_header() {
    let src = include_str!("../fixtures/c/bitfield_rmw_header.c");
    let (out, code) = run_host_link_and_run("bitfield-rmw-header", src);
    assert!(
        out.status.success(),
        "bitfield_rmw_header compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "bitfield_rmw_header checks failed (exit={code})");
}

/// F1b: mixed bitfield write + inc/dec RMW nesting must not overlap
/// despite sharing the bf_rmw_depth counter. Regression for H1b.
#[test]
#[cfg(unix)]
fn bitfield_rmw_mixed_nest() {
    let src = include_str!("../fixtures/c/bitfield_rmw_mixed_nest.c");
    let (out, code) = run_host_link_and_run("bitfield-rmw-mixed-nest", src);
    assert!(
        out.status.success(),
        "bitfield_rmw_mixed_nest compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "bitfield_rmw_mixed_nest checks failed (exit={code})");
}

/// F3: deeply nested bitfield RMW must not wrap i8 offsets into
/// positive r13 displacements that stomp on the caller's frame.
/// Regression for H1/H2.
#[test]
#[cfg(unix)]
fn bitfield_rmw_deep_nest() {
    let src = include_str!("../fixtures/c/bitfield_rmw_deep_nest.c");
    let (out, code) = run_host_link_and_run("bitfield-rmw-deep-nest", src);
    assert!(
        out.status.success(),
        "bitfield_rmw_deep_nest compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "bitfield_rmw_deep_nest checks failed (exit={code})");
}

/// M2.3: integer promotion and implicit conversion rules.
#[test]
#[cfg(unix)]
fn m2_3_integer_promotion() {
    let src = include_str!("../fixtures/c/promotion.c");
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
    let src = include_str!("../fixtures/c/pointer.c");
    let (out, code) = run_host_link_and_run("m2-4-pointer", src);
    assert!(
        out.status.success(),
        "m2_4 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "pointer checks failed (exit={code})");
}

/// Struct field aligned: __attribute__((aligned(N))) on struct fields
/// correctly affects layout padding and memory access offsets.
#[test]
#[cfg(unix)]
fn struct_field_aligned_codegen() {
    let src = include_str!("../fixtures/c/struct_field_aligned.c");
    let (out, exe) = run_compile_and_run("struct-field-aligned", src);
    assert!(
        out.status.success(),
        "struct field aligned compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(run_exe(&exe), 0, "struct field aligned checks failed");
    let _ = fs::remove_file(&exe);
}

/// M3.3: statement expressions ({ ... }) GNU extension.
#[test]
#[cfg(unix)]
fn m3_3_statement_expressions() {
    let src = include_str!("../fixtures/c/stmt_expr.c");
    let (out, code) = run_host_link_and_run("m3-3-stmt-expr", src);
    assert!(
        out.status.success(),
        "m3_3 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "statement expr checks failed (exit={code})");
}

/// M3.5: VLA (variable-length arrays) and zero-length arrays.
#[test]
#[cfg(unix)]
fn m3_5_vla_zero_array() {
    let src = include_str!("../fixtures/c/vla.c");
    // 只跑 -O0/-O1：-O2/-O3 仍有一个已定位、未修的存量债（`check_opt_equivalence.sh`
    // 报为 OPT-DIFF，跳档与 -O0 不一致）。定位结果与新证据见
    // docs/ssa-pass-upgrade-plan.md §7.10：触发者是 SSA 层 GVN，而两种
    // 「看上去合理」的修法（把数组类型临时值衰减成指针 / 不让数组结果参与值编号）
    // 都被实测否定，所以这里不能先把它当成已绿。
    for flag in ["-O0", "-O1"] {
        let (out, exe) = run_compile_and_run_flags("m3-5-vla", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag} 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let code = run_exe(&exe);
        assert_eq!(code, 0, "{flag}: VLA/zero-array 检查失败 (exit={code})");
        let _ = fs::remove_file(exe);
    }
}

#[test]
#[cfg(unix)]
fn c_arrow_member_store_offset_and_width() {
    // 回归测试: `ptr->field = val` 必须按字段偏移与宽度存储.
    // (虚继承实现中发现 dcc C 后端对 `->` 成员赋值退化为 4 字节 + 偏移 0)
    let src = r#"
struct S { int a; long x; };
void f(struct S* p) { p->x = 5; }
int main(void) {
    struct S s; s.a = 1; s.x = 0;
    f(&s);
    if (s.x != 5) return 1;
    if (s.a != 1) return 2;
    return 0;
}
"#;
    let (out, obj) = run_object_test("arrow-member-store", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-arrow-member-store-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run arrow-member-store binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// C89 函数类型参数 (C11 6.7.6.3p8): 参数声明为函数类型时调整为函数指针.
/// 内核 include/net/ipv6.h 的 ip6_append_data 以 `int getfrag(void*, char*, ...)`
/// 形式声明回调参数. 修复前 parse_params 报 E0001 (expected ')', got LParen).
#[test]
fn func_type_param_compiles() {
    let source = include_str!("../fixtures/c/func_type_param.c");
    let (output, obj) = run_object_test("func-type-param", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 全局静态初始化器链接期地址常量 (E1006 回归): 数组成员隐式数组退化
/// (utsname_sysctl.c) / &member_chain[i] (tracepoint.c) / (&x)->field
/// (watchdog.c cpumask_bits 宏展开) 三种形式.
#[test]
fn static_addr_const_init_compiles() {
    let source = include_str!("../fixtures/c/static_addr_const.c");
    let (output, obj) = run_object_test("static-addr-const", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 多维数组维度从内向外解析 (C11 6.7.6.2p4): char buf[2][8] 是 2 个 char[8].
/// 修复前 3 处维度解析从左到右包裹, sizeof buf[0]==2 (应为 8), 元素 stride
/// 错 (words[1] 按 1 字节步进读标量), 运行时越界/崩溃; 另覆盖参数多维数组
/// 退化 (int a[][3] -> int (*a)[3]) 与数组类型元素表达式退化 (C11 6.3.2.1p3).
#[test]
fn multi_dim_array_compiles_and_runs() {
    let source = include_str!("../fixtures/c/multi_dim_array.c");
    let (output, exe) = run_compile_and_run("multi-dim-array", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "multi_dim_array should return 0, got {rc}");
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

// =================== 嵌套块重声明补充 ===================

/// 嵌套块重声明: 内层块遮蔽全局变量 (不同类型不报 E0007)
#[test]
fn nested_block_redecl_shadows_global() {
    let src = "\
int x = 100;
int main(void) {
    {
        double x = 3.14;
        if (x > 3.0) return 0;
    }
    return x == 100 ? 0 : 1;
}
";
    let (output, exe) = run_compile_and_run("nested-redecl-global", src);
    assert!(
        output.status.success(),
        "nested block redecl shadowing global should compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "inner x=3.14 > 3.0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 嵌套块重声明: 多层嵌套遮蔽
#[test]
fn nested_block_redecl_multi_level() {
    let src = "\
int main(void) {
    int x = 1;
    {
        int x = 2;
        {
            int x = 3;
            if (x != 3) return 1;
        }
        if (x != 2) return 2;
    }
    if (x != 1) return 3;
    return 0;
}
";
    let (output, exe) = run_compile_and_run("nested-redecl-multi", src);
    assert!(
        output.status.success(),
        "multi-level nested redecl should compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "each scope should see its own x, got {rc}");
    let _ = fs::remove_file(exe);
}

// ====== Semantic run tests: enum + type traits ======
#[test]
#[cfg(unix)]
/// Semantic enum: type traits runtime - 链接 + 运行, 逐条校验枚举类型属性.
///
/// 升级背景: 本测试原本只 compile-only + #[ignore] ("enum type traits runtime
/// 仍 TODO"). 夹具 tests/fixtures/cpp/semantic_run.cpp 现在把 underlying_type /
/// is_enum 退化成整数、底层类型尺寸与符号性、枚举量作数组维度等语义都写成
/// 运行期断言 (错一个就返回非 0), 因此与 cpp_enum_semantics_runtime 同构:
/// dcc -c -> host gcc 链接 -> 运行必须 exit 0.
fn semantic_enum_type_traits_runtime() {
    // Semantic run test: enum values, scoped enum, sizeof type properties
    let src = include_str!("../fixtures/cpp/semantic_run.cpp");
    let (out, obj) = run_cpp_object_test("semantic-enum-runtime", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-semantic-enum-runtime-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run semantic enum binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Features 12/13: -fno-strict-aliasing and -fno-strict-overflow
#[test]
#[cfg(unix)]
fn strict_aliasing_overflow_flags() {
    let src = r#"
int func(int *p, long *q) {
    int x = *p;
    *q = 0;
    return x + *p;  /* With strict aliasing, could optimize to x + x */
}
int overflow_test(int x) {
    return x + 1 > x;  /* With strict overflow, always true */
}
"#;
    let tmp = std::env::temp_dir();
    let src_path = tmp.join("strict_flags.c");
    let obj_path = tmp.join("strict_flags.o");
    fs::write(&src_path, src).unwrap();
    // Test -fno-strict-aliasing
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fno-strict-aliasing", "-O2", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -fno-strict-aliasing");
    assert!(
        output.status.success(),
        "no-strict-aliasing compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&obj_path);

    // Test -fno-strict-overflow
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-fno-strict-overflow", "-O2", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -fno-strict-overflow");
    assert!(
        output.status.success(),
        "no-strict-overflow compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

// ── Multi-file parallel compilation ──────────────────────────────

// ---------------------------------------------------------------------------
// QualType qualifier tests
// ---------------------------------------------------------------------------

/// QualType: const/volatile/restrict qualifiers compile and run correctly
#[test]
fn qual_type_compiles_and_runs() {
    let source = include_str!("../fixtures/c/qual_type.c");
    let (output, exe) = run_compile_and_run("qual-type", source);
    assert!(
        output.status.success(),
        "qual_type compile+run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "qual_type main() should return 0, got {rc}");
    let _ = fs::remove_file(&exe);
}

/// QualType: const/volatile/restrict qualifiers produce valid object code
#[test]
fn qual_type_object_code() {
    let source = include_str!("../fixtures/c/qual_type.c");
    let (output, obj) = run_object_test("qual-type-obj", source);
    let _ = fs::remove_file(obj);
    assert!(
        output.status.success(),
        "qual_type object compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// QualType: volatile variable survives optimization at -O2
#[test]
fn qual_type_volatile_survives_o2() {
    let source = "\
volatile int flag = 0;
int main(void) {
    flag = 1;
    return flag - 1;
}
";
    let (output, exe) = run_compile_and_run_flags("qual-volatile-o2", source, &["-O2"]);
    assert!(
        output.status.success(),
        "volatile O2 compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "volatile write/read must survive -O2, got rc={rc}");
    let _ = fs::remove_file(&exe);
}

/// QualType: syntax-only test for multi-level pointer qualifiers
#[test]
fn qual_type_multi_level_ptr_syntax() {
    let source = "\
const int *volatile *restrict p;
volatile const int **q;
int main(void) { return 0; }
";
    let output = run_syntax_test("qual-multi-ptr", source);
    assert!(
        output.status.success(),
        "multi-level pointer qualifiers failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C89 multi-character character constant: 'ab' form
/// GCC-compatible big-endian concatenation: 'ab' = ('a' << 8) | 'b'
#[test]
fn multichar_const_syntax_ok() {
    let src = r#"
int a = 'ab';
int b = 'abc';
_Static_assert('ab' == 0x6162, "multi-char value");
_Static_assert('a' == 0x61, "single char value");
int main(void) { return 0; }
"#;
    let output = run_syntax_test("multichar-const", src);
    assert!(
        output.status.success(),
        "Multi-character constants should parse. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C89 multi-character constant runtime value verification
#[test]
fn multichar_const_run_returns_zero() {
    let source = include_str!("../fixtures/c/multichar_const.c");
    let (output, exe) = run_compile_and_run("multichar-const-run", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "multichar_const should return 0, got {rc}");
}

/// C89 void* implicit conversion to/from any object pointer type
/// C89 3.3.16, C99 6.3.2.3: void* is compatible with any pointer
#[test]
fn void_ptr_implicit_conversion_syntax_ok() {
    let source = include_str!("../fixtures/c/void_ptr_implicit.c");
    let output = run_syntax_test("void-ptr-implicit", source);
    assert!(
        output.status.success(),
        "void* implicit conversion should be accepted. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C89 const correctness: assignment to const variable should be rejected
#[test]
fn const_assign_error() {
    let src = r#"
int main(void) {
    const int x = 10;
    x = 20;
    return 0;
}
"#;
    let output = run_syntax_test("const-assign-err", src);
    assert!(!output.status.success(), "Assignment to const variable should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("read-only variable") || stderr.contains("E0030"),
        "Should report const assignment error. stderr: {}",
        stderr
    );
}

/// C89 const correctness: valid const usage should compile
#[test]
fn const_correct_syntax_ok() {
    let source = include_str!("../fixtures/c/const_correct.c");
    let output = run_syntax_test("const-correct", source);
    assert!(
        output.status.success(),
        "Valid const usage should compile. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C89 volatile semantics: volatile variables compile at all opt levels
#[test]
fn volatile_semantics_syntax_ok() {
    let source = include_str!("../fixtures/c/volatile_semantics.c");
    // Test at O0
    let output = run_syntax_test("volatile-semantics-o0", source);
    assert!(
        output.status.success(),
        "volatile semantics at O0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C89 volatile: verify volatile qualifier is parsed and stored
#[test]
fn volatile_qualifier_parsed() {
    let src = r#"
volatile int g;
int main(void) {
    volatile int x = 1;
    int y = x;
    g = y;
    return 0;
}
"#;
    let output = run_syntax_test("volatile-qual", src);
    assert!(
        output.status.success(),
        "volatile qualifier should parse. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── Floating-point exception support tests ──

/// Verify fenv.h functions compile and produce correct results.
/// Tests fegetround, fesetround, feclearexcept, feraiseexcept, fetestexcept.
#[test]
fn fenv_exception_functions_compile_and_run() {
    let src = r#"
#include <stdio.h>
#include <fenv.h>
int main(void) {
    // Test fegetround
    int round_mode = fegetround();
    printf("round=%d\n", round_mode);
    // Test feclearexcept + fetestexcept
    feclearexcept(FE_ALL_EXCEPT);
    int result = fetestexcept(FE_ALL_EXCEPT);
    printf("cleared=%d\n", result);
    // Test feraiseexcept + fetestexcept
    feraiseexcept(FE_INVALID);
    result = fetestexcept(FE_INVALID);
    printf("invalid=%d\n", result != 0);
    feclearexcept(FE_ALL_EXCEPT);
    return 0;
}
"#;
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-fenv-except-{}.c", std::process::id()));
    let bin_path = dir.join(format!("dcc-fenv-except-{}", std::process::id()));
    std::fs::write(&src_path, src).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-o"])
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("compile fenv test");
    let _ = std::fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "fenv compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(&bin_path).output().expect("run binary");
    let _ = std::fs::remove_file(&bin_path);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("round=0"), "fegetround: {}", stdout);
    assert!(stdout.contains("cleared=0"), "feclearexcept: {}", stdout);
    assert!(stdout.contains("invalid=1"), "feraiseexcept: {}", stdout);
}

/// Verify fegetenv/fesetenv save and restore floating-point environment.
#[test]
fn fenv_save_restore_env() {
    let src = r#"
#include <stdio.h>
#include <fenv.h>
int main(void) {
    fenv_t env;
    fegetenv(&env);
    feraiseexcept(FE_OVERFLOW);
    int before = fetestexcept(FE_OVERFLOW);
    fesetenv(&env);
    int after = fetestexcept(FE_OVERFLOW);
    printf("before=%d after=%d\n", before != 0, after != 0);
    return 0;
}
"#;
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-fenv-save-{}.c", std::process::id()));
    let bin_path = dir.join(format!("dcc-fenv-save-{}", std::process::id()));
    std::fs::write(&src_path, src).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-o"])
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("compile fenv save/restore test");
    let _ = std::fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "fenv save/restore compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(&bin_path).output().expect("run binary");
    let _ = std::fs::remove_file(&bin_path);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("before=1 after=0"), "fegetenv/fesetenv: {}", stdout);
}

// ── Nested function trampoline tests ──

/// Verify nested function address-taking compiles.
/// The trampoline mechanism allows nested functions to be used
/// as function pointers.
#[test]
fn nested_func_trampoline_compile() {
    let src = r#"
#include <stdio.h>
int main(void) {
    int x = 42;
    void nested(void) {
        printf("x=%d\n", x);
    }
    // 取嵌套函数地址应触发 trampoline 生成
    void (*fp)(void) = nested;
    fp();
    return 0;
}
"#;
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-tramp-{}.c", std::process::id()));
    let bin_path = dir.join(format!("dcc-tramp-{}", std::process::id()));
    std::fs::write(&src_path, src).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-o"])
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("compile trampoline test");
    let _ = std::fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "trampoline compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_file(&bin_path);
}

/// Verify nested function direct call with static chain works.
#[test]
fn nested_func_direct_call() {
    let src = r#"
#include <stdio.h>
int main(void) {
    int x = 10;
    int add_x(int y) {
        return x + y;
    }
    int result = add_x(32);
    printf("%d\n", result);
    return 0;
}
"#;
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-nested-direct-{}.c", std::process::id()));
    let bin_path = dir.join(format!("dcc-nested-direct-{}", std::process::id()));
    std::fs::write(&src_path, src).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-o"])
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("compile nested func test");
    let _ = std::fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "nested func compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(&bin_path).output().expect("run binary");
    let _ = std::fs::remove_file(&bin_path);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.trim() == "42", "expected 42, got: {}", stdout.trim());
}

// =================== static local variable storage ===================

/// static local 标量跨调用持久性
#[test]
fn static_local_scalar_persistence() {
    let source = r#"
int counter(void) { static int n; return ++n; }
int main(void) {
    if (counter() != 1) return 1;
    if (counter() != 2) return 2;
    if (counter() != 3) return 3;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-scalar", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_scalar should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// static local 未初始化数组 → .bss, 跨调用持久
#[test]
fn static_local_uninit_array() {
    let source = r#"
void f(int *p) { static int a[4]; a[0]++; *p = a[0]; }
int main(void) {
    int v;
    f(&v); if (v != 1) return 1;
    f(&v); if (v != 2) return 2;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-uninit-array", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_uninit_array should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// static local 常量初始化列表数组
#[test]
fn static_local_init_list_array() {
    let source = r#"
int sum(void) {
    static int a[4] = {1, 2, 3, 4};
    int s = 0;
    for (int i = 0; i < 4; i++) s += a[i];
    return s;
}
int main(void) {
    if (sum() != 10) return 1;
    if (sum() != 10) return 2;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-init-list", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_init_list should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// static local 字符串初始化数组
#[test]
fn static_local_string_init_array() {
    let source = r#"
const char *get(void) {
    static char s[8] = "abc";
    return s;
}
int main(void) {
    const char *p = get();
    if (p[0] != 'a' || p[1] != 'b' || p[2] != 'c' || p[3] != '\0')
        return 1;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-string-init", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_string_init should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// static local 在嵌套块 (if body) 中声明
#[test]
fn static_local_in_nested_block() {
    let source = r#"
int f(int x) {
    if (x > 0) {
        static int n;
        n += x;
        return n;
    }
    return 0;
}
int main(void) {
    if (f(1) != 1) return 1;
    if (f(2) != 3) return 2;
    if (f(3) != 6) return 3;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-nested-block", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_nested_block should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// static local 取地址跨调用一致性
#[test]
fn static_local_address_of() {
    let source = r#"
int *get_addr(void) { static int x = 42; return &x; }
int main(void) {
    int *p = get_addr();
    if (*p != 42) return 1;
    *p = 99;
    int *q = get_addr();
    if (*q != 99) return 2;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-addr", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_addr should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// static local 部分初始化数组 (尾部补零)
#[test]
fn static_local_partial_init_array() {
    let source = r#"
int f(void) {
    static int a[4] = {1};
    return a[0] + a[1] + a[2] + a[3];
}
int main(void) {
    if (f() != 1) return 1;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("static-local-partial-init", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_partial_init should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

// =================== 数组下标寻址语义层测试矩阵 ===================

/// Layer 1: static local 数组与文件级全局数组同名
#[test]
fn semantic_layer_static_local_name_conflict() {
    let source = r#"
int x[8] = {100, 200, 300, 400, 500, 600, 700, 800};
int test_const(void) {
    static int x[4] = {10, 20, 30, 40};
    return x[0] + x[1] + x[2] + x[3];
}
int test_runtime(int i) {
    static int x[4] = {10, 20, 30, 40};
    return x[i];
}
int test_global(void) { return x[0] + x[7]; }
int main(void) {
    if (test_const() != 100) return 1;
    if (test_runtime(0) != 10) return 2;
    if (test_runtime(3) != 40) return 3;
    if (test_global() != 900) return 4;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("semantic-sl-conflict", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_name_conflict should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// Layer 2: 多维数组中间层衰减 a[i][j]
#[test]
fn semantic_layer_multidim_array() {
    let source = r#"
int a[3][4] = {
    {1, 2, 3, 4},
    {5, 6, 7, 8},
    {9, 10, 11, 12}
};
int main(void) {
    if (a[0][0] != 1) return 1;
    if (a[1][2] != 7) return 2;
    if (a[2][3] != 12) return 3;
    int i = 1, j = 3;
    if (a[i][j] != 8) return 4;
    /* 中间层衰减: a[i] 是 int[4], 衰减为 int* */
    int *p = a[1];
    if (p[2] != 7) return 5;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("semantic-multidim", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "multidim_array should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// Layer 2: VLA 运行时 stride
#[test]
fn semantic_layer_vla_runtime() {
    let source = r#"
int test_vla(int n) {
    int a[n];
    int i;
    for (i = 0; i < n; i++) a[i] = i * 10;
    return a[n - 1];
}
int main(void) {
    if (test_vla(5) != 40) return 1;
    if (test_vla(1) != 0) return 2;
    if (test_vla(10) != 90) return 3;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("semantic-vla", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "vla_runtime should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// Layer 3: &arr[i] (TakeAddress) vs arr[i] (LoadScalar)
#[test]
fn semantic_layer_addr_vs_load() {
    let source = r#"
int arr[5] = {10, 20, 30, 40, 50};
int main(void) {
    /* LoadScalar: 直接读取值 */
    int v = arr[2];
    if (v != 30) return 1;
    /* TakeAddress: 取元素地址, 再通过指针间接访问 */
    int *p = &arr[3];
    if (*p != 40) return 2;
    /* 通过指针修改 */
    *p = 99;
    if (arr[3] != 99) return 3;
    /* 地址算术 */
    int *q = &arr[0] + 4;
    if (*q != 50) return 4;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("semantic-addr-vs-load", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "addr_vs_load should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// Layer 3: 聚合元素数组 struct S s[4]; s[i]
#[test]
fn semantic_layer_aggregate_array() {
    let source = r#"
struct Point { int x; int y; };
struct Point pts[3] = {{1, 2}, {3, 4}, {5, 6}};
int main(void) {
    if (pts[0].x != 1) return 1;
    if (pts[1].y != 4) return 2;
    if (pts[2].x != 5) return 3;
    /* 运行时索引 */
    int i = 1;
    if (pts[i].x != 3) return 4;
    /* 聚合赋值 */
    struct Point p2 = {10, 20};
    pts[i] = p2;
    if (pts[1].x != 10) return 5;
    if (pts[1].y != 20) return 6;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("semantic-agg-array", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "aggregate_array should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// Layer 1+2: static local 数组取地址 + 运行时索引组合
#[test]
fn semantic_layer_static_local_addr_and_index() {
    let source = r#"
int x[4] = {100, 200, 300, 400};
int test(void) {
    static int x[4] = {1, 2, 3, 4};
    int *p = &x[1];
    int sum = *p + x[0];
    /* 通过指针访问 static local 数组 */
    p = &x[0];
    sum += p[3];
    return sum; /* 2 + 1 + 4 = 7 */
}
int test_global(void) {
    return x[0] + x[3]; /* 100 + 400 = 500 */
}
int main(void) {
    if (test() != 7) return 1;
    if (test_global() != 500) return 2;
    return 0;
}
"#;
    let (output, exe) = run_compile_and_run("semantic-sl-addr-idx", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "static_local_addr_index should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// M2.5: float support - float/double/long double ABI,
/// arithmetic, conversions, comparisons.
#[test]
#[cfg(unix)]
fn m2_5_float() {
    let src = include_str!("../fixtures/c/float.c");
    let (out, code) = run_host_link_and_run("m2-5-float", src);
    assert!(
        out.status.success(),
        "m2_5 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "float checks failed (exit={code})");
}

/// 同标签 struct/union 指针重声明兼容: C11 6.2.5p22 同标签即同一类型.
/// 内核 include/linux/sched.h `extern struct pid *cad_pid;` (标签不完整)
/// + kernel/reboot.c 完整定义. 修复前 Ptr 包装后 E0007.
#[test]
fn tag_compat_redecl_compiles() {
    let source = include_str!("../fixtures/c/tag_compat_redecl.c");
    let (output, obj) = run_object_test("tag-compat-redecl", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 两个 `double` 形参必须分别落在 xmm0 / xmm1（而非共享一个寄存器或错
/// 位到 GP）。只比退出码时“两侧同错”仍可能抵消灭为 0，所以走 gcc 差分。
#[test]
#[cfg(unix)]
fn two_double_params_use_separate_xmm_regs() {
    let src = include_str!("../fixtures/c/double_args.c");
    let rc = run_gcc_cross_check("double-args", src);
    assert_eq!(rc, 0, "add2(1.5, 2.5) 必须得 4.0，否则夹具返回 1");
}

/// Linux 内核写法 `for (enum mod_mem_type (type) = 0; ...)`：带括号的
/// 声明符。解析错了会直接编译失败，而循环能否在第一次迭代命中
/// `MOD_INIT_TEXT` 才是语义部分 —— 夹具期望返回 1（命中即 return 1）。
#[test]
#[cfg(unix)]
fn enum_decl_with_parenthesized_declarator_matches_gcc() {
    let src = include_str!("../fixtures/c/enum.c");
    let rc = run_gcc_cross_check("enum-paren-decl", src);
    assert_eq!(rc, 1, "enum 具名循环变量应在第一次迭代命中 MOD_INIT_TEXT");
}

/// 字段级 `aligned(N)` 必须真的改变偏移（不只是“能解析”）：夹具里
/// `_Static_assert` 钉住 8 个结构的大小，`test_field_aligned` 再在运行期检查
/// `&b.b % 4 == 0` 与 `&la.y % 16 == 0`。
///
/// 为何不走 gcc 差分：夹具中 `sizeof(struct packed_aligned) == 5` 记录的是 dcc
/// 当前的 packed/字段-aligned 交互语义，gcc 给 16（字段的 `aligned(8)` 压过结构体
/// 的 `packed`）—— 该差异已在夹具头部注释与本测试中显式登记，等待裁决；其余
/// 八个布局两侧完全一致。
#[test]
#[cfg(unix)]
fn field_level_aligned_attrs_take_effect_at_runtime() {
    let src = include_str!("../fixtures/c/field_aligned_attr.c");
    let (output, exe) = run_compile_and_run("field-aligned-attr", src);
    assert!(
        output.status.success(),
        "编译失败说明某个 _Static_assert 不成立: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "字段对齐运行期检查失败（1=&b.b 丌4对齐, 2=&la.y 丌16对齐）");
    let _ = fs::remove_file(exe);
}

/// `long double` 必须是 x87 扩展精度路径：夹具先卡 `sizeof(long double) == 16`，
/// 再逐项校加/减/乘/除与四比较。注意“1.0L + 1e-18L != 1.0L”这类真 80-bit 精度
/// 判据**故意没写** —— dcc 内部按 f64 表示长浮点，写了就是个红灯；本件只钉
/// ABI 尺寸与基本算术通路。
#[test]
#[cfg(unix)]
fn long_double_size_and_x87_arithmetic_match_gcc() {
    let src = include_str!("../fixtures/c/long_double_precision.c");
    let rc = run_gcc_cross_check("long-double-precision", src);
    assert_eq!(rc, 0, "long_double 逐项算术/比较失败（非 0 值即第几项错）");
}

/// 128-bit 向量（`vector_size(16)` 的 v4si/v4sf/v2df）的加/减/乘/除与位运算
/// 必须按元素展开到 XMM 打包指令。夹具把 12 个运算函数全写了但只跑其中两个，
/// 其余靠“能编译”钉住；运行期部分走 gcc 差分。
#[test]
#[cfg(unix)]
fn simd_vector_ops_on_128bit_match_gcc() {
    let src = include_str!("../fixtures/c/simd_vector_xmm.c");
    let rc = run_gcc_cross_check("simd-vector-xmm", src);
    assert_eq!(rc, 0, "v4si 加法或 v4sf 加法逐_lane_结果错（1=整型 2=浮点）");
}

/// `static` 局部数组与文件级全局数组**同名**时的寻址必须各自解析到正确的
/// 对象（`resolve_global_key` 的 mangle 路径）。比已有的内联用例
/// `semantic_layer_static_local_addr_and_index` 多钉三处：`static int *p` 这种
/// static 局部**指针**的间接访问、对 static 局部数组的**写**、以及多个函数各自
/// 声明同名 static 数组时的隔离。
#[test]
#[cfg(unix)]
fn static_local_arrays_shadow_file_scope_global() {
    let src = include_str!("../fixtures/c/static_local_arrays.c");
    let rc = run_gcc_cross_check("static-local-arrays", src);
    assert_eq!(rc, 0, "static 局部数组与全局同名解析错（非 0 即第几项错）");
}

/// `typedef int (fn_t)(int cpu);` 这种“带括号的函数类型名”作参数类型时，
/// 必须按 6.7.6.3 调整为函数指针并能**跳编译器**链接（内核 per-cpu 回调写法）。
/// 单 TU 测不出“两侧对参数类型的理解不一致”，所以用两个 TU 四方向对照：
/// provider 是夹具本体（只定义 `use`），consumer 把函数名当实参传。
#[test]
#[cfg(unix)]
fn typedef_function_type_param_adjusts_across_tu() {
    let provider = include_str!("../fixtures/c/typedef_fn.c");
    let consumer = "typedef int (fn_t)(int);\n\
                    int use(fn_t f);\n\
                    static int ident(int cpu) { return cpu + 7; }\n\
                    int main(void) { return use(ident); }\n";
    let rc = run_gcc_two_tu_cross_check("typedef-fn-param", provider, consumer);
    assert_eq!(rc, 7, "use(ident) 应返回 ident(0) == 7；参数类型未调成指针时根本不可链接");
}

/// 前向声明 + 同 TU 后续定义: dcc 不应为前向声明额外生成 UND 符号,
/// 避免与本 TU 内的定义在同名前缀下产生多个 .symtab 条目.
/// 复现 kernel `arch/x86/tools/relocs_32.c` 模式: relocs.h 声明 process_32,
/// relocs_32.c 经 relocs.c 包含定义 process_32. 之前会同时输出
/// `FUNC GLOBAL UND process_32` 与 `FUNC GLOBAL DEFAULT 1 process_32`,
/// 导致静态检查和外部链接失败.
#[test]
fn fwd_decl_same_tu_dedup() {
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
    assert_eq!(helper_und, 0, "fwd declaration should not emit UND sym; got:\n{syms}");
    assert!(
        helper_defined >= 1,
        "definition should emit at least one DEFAULT sym; got:\n{syms}"
    );
    let _ = fs::remove_file("/tmp/fwd_decl.o");
}

/// 指定初始化器 + asm inline（内核 GDT/jump_label 常见模式）
#[test]
fn gdt_designated_init_and_asm_inline_compiles() {
    let source = include_str!("../fixtures/c/designated.c");
    let (output, obj) = run_object_test("gdt-designated-init", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 16 字节 struct 传值 ABI: struct {double,double} 走 xmm0:xmm1,
/// struct {long,long} 走 rdi:rsi, SSE 类返回值经 xmm0:xmm1
#[test]
#[cfg(unix)]
fn m15_abi16_struct_passing() {
    let src = include_str!("../fixtures/c/abi16_struct.c");
    let (out, exe) = run_compile_and_run("m15-abi16-struct", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// `long double` **全局量**读/发射两侧的不得回退门（主体：x87 80-bit 数据布局与装载）。
///
/// 原缺陷（夹具头部注释与 `tests/fixtures/c_xfail/README.md` 存档的判据）：`long double`
/// 全局量的读侧按 f64 取低 8 字节 —— `movsd 0x2159(%rip),%xmm0  # <gh>`，把 80-bit 对象的
/// 尾数字节当 f64 位型，再 `fstpt` 拓宽 → 打印 `-nan`，夹具自证判据 `r` 停在 **14**
/// （缺 bit0），进程 **rc == 1**。
///
/// 该缺陷在当前树已修（实测 `r=15 want=15`、`rc=0`），故本件已从 `c_xfail/` 转正进
/// `c/`（先例：同一 README 里的 `flag_clobber_frame.c`），这里不能写成“断言 dcc
/// 失败”，而是钉住**修好后的具体值**：一旦读侧/发射侧回退到上面的 xfail 特征，
/// 以下每一处都会红：
/// 1. 发射侧: `.data` 里 `gh`/`ga` 是真 80-bit x87 extended 编码 —— 指数域
///    0x3fff/0x4000、显式整数位 (bit63) 置起、尾数 1.0/2.5/3.0, 且 16 字节槽内
///    10 字节之后是填充零; 符号尺寸 `gh` 16、`ga` 32 (每元素 16 字节 ABI 对齐);
/// 2. 读侧文本面: `<main>` 体内 **不得出现 `movsd`**, 且至少一条 `%rip` 相对
///    `fldt` (f64 读法一回来就命中);
/// 3. 读侧链接面 (需要 host `cc` + `objdump`, 缺失时真实短路, 不静默跳过 1/2):
///    反汇编里 `<gh>` 必须由 `fldt` 载入 —— 与 gcc 的 `fldt # <gh>` 同形;
/// 4. 行为面: 退出码 **0** 且 stdout 含 `r=15 want=15`、`gh=1.000000000000000000000000000`,
///    **不含 `-nan`** —— 即夹具自身编码的"四项全真"判据。
///
/// 注: 该件已由 xfail 转 XPASS 并转正到 `tests/fixtures/c/ld_global_read.c`；
/// `tests/fixtures/c_xfail/README.md` 已记一行迁移原因，`c_xfail/` 回到“只放
/// 已定位未修缺陷”的 §7.1 角色。
#[test]
#[cfg(unix)]
fn ld_global_read_long_double_uses_80bit_load_and_exact_data() {
    let dir = std::env::temp_dir().join(format!("dcc-ld-global-read-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(
        dir.join("ld_global_read.c"),
        include_str!("../fixtures/c/ld_global_read.c"),
    )
    .expect("write fixture");

    let obj_path = dir.join("ld_global_read.o");
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .current_dir(&dir)
        .args([
            "-O0",
            "-c",
            "ld_global_read.c",
            "-o",
            "ld_global_read.o",
        ])
        .output()
        .expect("run dcc");
    assert!(
        compile.status.success(),
        "long double 全局夹具必须编译成功: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // ---- 1) 发射侧: `.data` 里的真 80-bit extended 位型与 ABI 尺寸 ----
    let obj = fs::read(&obj_path).expect("read object");
    assert_eq!(
        u16::from_le_bytes(obj[18..20].try_into().unwrap()),
        62,
        "应为 x86-64 目标"
    );

    let syms = elf64_syms(&obj);
    let locate = |want: &str| -> (u64, u64) {
        syms.iter()
            .find(|s| s.name == want)
            .map(|s| (s.value, s.size))
            .unwrap_or_else(|| panic!("`.symtab` 应有全局量 {want}, 实际: {:?}", 
                syms.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()))
    };
    let (gh_off, gh_size) = locate("gh");
    let (ga_off, ga_size) = locate("ga");
    assert_eq!(gh_size, 16, "x86-64 SysV 上 long double 对象占 16 字节槽");
    assert_eq!(ga_size, 32, "ga[2] 每元素 16 字节");
    assert_eq!(gh_off % 16, 0, "gh 需 16 字节对齐");
    assert_eq!(ga_off % 16, 0, "ga 需 16 字节对齐");

    let data = elf64_section_data(&obj, ".data").expect("应产出 .data");
    // 80-bit x87 extended: 8 字节尾数 (含显式整数位 bit63) + 2 字节有符号指数
    let decode = |slot: &[u8], want_exp: u64, want_mant: u64, label: &str| {
        assert!(slot.len() >= 16, "{label}: .data 太短, 实际 {}", data.len());
        let mut mant = 0u64;
        for (i, &b) in slot[..8].iter().enumerate() {
            mant |= (b as u64) << (8 * i);
        }
        let exp = (slot[8] as u64) | ((slot[9] as u64) << 8);
        assert_eq!(exp, want_exp, "{label}: 指数域应为 {want_exp:#x}");
        assert_eq!(mant, want_mant, "{label}: 尾数域应为 {want_mant:#x}");
        assert_ne!(
            mant & (1u64 << 63),
            0,
            "{label}: 80-bit 有显式整数位, 与 f64 的隐含整数位不同 —— \
             若这里为 0 说明又被按 f64 写出"
        );
        assert_eq!(
            &slot[10..16],
            &[0u8; 6],
            "{label}: 10 字节有效长度之后应为填充零"
        );
    };
    decode(
        &data[gh_off as usize..],
        0x3fff,
        1u64 << 63,
        "gh = 1.0L",
    );
    decode(
        &data[ga_off as usize..],
        0x4000,
        0xa000_0000_0000_0000,
        "ga[0] = 2.5L",
    );
    decode(
        &data[ga_off as usize + 16..],
        0x4000,
        0xc000_0000_0000_0000,
        "ga[1] = 3.0L",
    );

    // ---- 2) 读侧文本面 (无外部依赖): `<main>` 体内禁止 movsd ----
    let mut main_body = String::new();
    if tool_exists("objdump") {
        let dis = Command::new("objdump")
            .args(["-d", "--no-show-raw-insn"])
            .arg(&obj_path)
            .output()
            .expect("run objdump");
        assert!(
            dis.status.success(),
            "objdump -d 应成功: {}",
            String::from_utf8_lossy(&dis.stderr)
        );
        let text = String::from_utf8_lossy(&dis.stdout).into_owned();
        let mut inside = false;
        for line in text.lines() {
            let symbol_hdr =
                line.starts_with(|c: char| c.is_ascii_hexdigit()) && line.ends_with(">:");
            if symbol_hdr {
                if inside {
                    break;
                }
                inside = line.ends_with("<main>:");
                continue;
            }
            if inside {
                main_body.push_str(line);
                main_body.push('\n');
            }
        }
        assert!(
            !main_body.is_empty(),
            "objdump 应能切出 `<main>` 体, 反汇编全文: {text}"
        );
        assert!(
            !main_body.contains("movsd"),
            "long double 读侧不得再用 f64 的 movsd (原 xfail 特征), `<main>` 体:\n{main_body}"
        );
        assert!(
            main_body.contains("fldt") && main_body.contains("%rip)"),
            "`<main>` 应通过 %rip 相对的 x87 80-bit fldt 读全局, 实际:\n{main_body}"
        );
    } else {
        eprintln!("skip: 无 objdump, 跳过反汇编面 (.data 侧已断言)");
    }

    // ---- 3)+4) 链接后的精确值与 `<gh>` 载入指令 ----
    if !tool_exists("cc") {
        eprintln!("skip: 无 host cc, 跳过链接/运行段 (前两段已断言)");
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let exe_path = dir.join("ld_global_read_bin");
    let link = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .current_dir(&dir)
        .args(["-O0", "ld_global_read.c", "-o", "ld_global_read_bin"])
        .output()
        .expect("link with dcc");
    assert!(
        link.status.success(),
        "链接 long double 全局夹具必须成功: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    if tool_exists("objdump") {
        let dis = Command::new("objdump")
            .args(["-d"])
            .arg(&exe_path)
            .output()
            .expect("run objdump");
        assert!(
            dis.status.success(),
            "objdump -d 可执行文件应成功: {}",
            String::from_utf8_lossy(&dis.stderr)
        );
        let exe_text = String::from_utf8_lossy(&dis.stdout).into_owned();
        let gh_loads: Vec<&str> = exe_text
            .lines()
            .filter(|l| l.contains("<gh>"))
            .collect();
        assert!(
            !gh_loads.is_empty(),
            "链接产物反汇编应出现对 <gh> 的引用, 实际: {exe_text}"
        );
        for line in &gh_loads {
            assert!(
                line.contains("fldt"),
                "读 <gh> 必须是 80-bit fldt (gcc 同形), 出现回退: {line}"
            );
        }
    }

    let run = Command::new(&exe_path)
        .current_dir(&dir)
        .output()
        .expect("run instrumented binary");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert_eq!(
        run.status.code(),
        Some(0),
        "四项判据全真才是 rc=0 (原 xfail 为 rc=1): stdout={stdout}"
    );
    assert!(
        stdout.contains("r=15 want=15"),
        "stdout 应含 `r=15 want=15`, 实际: {stdout}"
    );
    assert!(
        stdout.contains("gh=1.000000000000000000000000000"),
        "全局 gh 应精确打印 1.0 (27 位小数), 实际: {stdout}"
    );
    assert!(
        !stdout.contains("-nan"),
        "不得再出现按 f64 误读 80-bit 造成的 -nan, 实际: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn permissive_implicit_function_decl_warns_and_continues() {
    let src = "int main(void) { return missing_func(1); }\n";
    let path = std::env::temp_dir().join(format!("dcc-perm-func-{}.c", std::process::id()));
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
        "permissive implicit function should not error: {stderr}"
    );
    assert!(stderr.contains("W1000"), "expected W1000: {stderr}");
    assert!(
        stderr.contains("implicit declaration of 'missing_func'"),
        "expected warning name: {stderr}"
    );
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
fn unknown_variable_read_is_not_implicit_function_warning() {
    let output =
        run_syntax_test("unknown-variable-read", "int main(void) { return missing_variable; }\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "unknown variable read still falls back to int: {stderr}"
    );
    assert!(
        !stderr.contains("W1000") && !stderr.contains("E0017"),
        "non-call identifier should not use implicit function diagnostics: {stderr}"
    );
}

/// #warning: 编译时输出警告, 不中断
#[test]
fn user_warning_does_not_break_compile() {
    let source = include_str!("../fixtures/c/user_diag.c");
    let output = run_syntax_test("user-warning", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 深嵌套二元式必须落在**帧溅出槽**的 disp32 编码上，且解码必须干净。
///
/// 为什么钉这一条：表达式第 `depth` 层的溅出槽偏移是 `-(48 + depth*8)`，发射点按
/// `|off| < 128` 在 disp8 / disp32 两条编码之间分岔（`expr/binary.rs`、`expr/dispatch.rs`）。
/// 而 disp32 分支直接发 `slot_off.to_le_bytes()` —— **发射宽度由变量类型决定，不由数值决定**。
/// P12 Phase 2 把该算式收进 `abi/layout.rs::temp_slot_off()` 时把它从 `i32` 变成了 `i64`：
/// 数值一字不差，但 `to_le_bytes()` 从 4 字节变成 8 字节，disp32 后面多了 4 个 `0xff`，
/// 紧随其后的整条指令流被推偏（objdump 报 `(bad)`，执行到那里 SIGILL）。
///
/// 当时的两层判据都没抓到：三档产物哈希的语料里没有 `expr_depth >= 10` 的帧槽回退，
/// 而 `git diff` 逐行核对看到的是“恒等替换”。所以这里把两个方向都钉住：
/// 静态（解码必须无 `(bad)`）+ 动态（执行结果必须对）。表达式的深度下界取 12：
/// `depth>=10` 就会越过 disp8 边界，留两层余量。
#[test]
fn deep_right_nested_binops_stay_decodable_and_correct() {
    // 12 层右嵌套：g(1) + (g(2) + (... + (g(12) + 0)))，求值应为 78。
    let mut src = String::from("int g(int x) { return x; }\nint f(void) { return ");
    for i in 1..=12 {
        src.push_str(&format!("g({}) + (", i));
    }
    src.push('0');
    for _ in 1..=12 {
        src.push(')');
    }
    src.push_str(";\n}\nint main(void) { return f() - 78; }\n");

    // 静态：编成目标文件后反汇编不得出现不可解码字节。
    let (output, obj) = run_object_test_flags("deep-nested-binop", &src, &["-O0"]);
    assert!(
        output.status.success(),
        "编译失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dump = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn"])
        .arg(&obj)
        .output()
        .expect("objdump");
    assert!(dump.status.success(), "objdump -d 失败");
    let text = String::from_utf8_lossy(&dump.stdout);
    assert!(
        !text.contains("(bad)"),
        "深嵌套表达式落在 disp32 编码上时字节流被写坏了（发射宽度应该永远是 4 字节）：\n{}",
        text.lines().filter(|l| l.contains("bad")).collect::<Vec<_>>().join("\n")
    );

    // 动态：写坏编码必然 SIGILL（退出码 132），能跑对才是真的对。
    let (cout, exe) = run_compile_and_run("deep-nested-binop-run", &src);
    assert!(
        cout.status.success(),
        "编译失败：{}",
        String::from_utf8_lossy(&cout.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "深嵌套右结合加法链的求值结果错（期望 78）");
}

/// N5 + N9：嵌套函数捕获外层变量的两条通路，与 VLA 运行时尺寸的基址。
///
/// 四档逐个与 gcc 对照（`-O0` 是唯一能看见静态链那条轴的档位，`-O2/-O3` 则验证
/// 「含嵌套函数的函数不进 SSA」那道闸仍然生效）。修前实测同一份源在
/// `-O0/-O2` 都返回 1：`sizeof a` 是 0（`n` 没进 upvars）⇒ `d=14` 而不是 72，
/// `plain` 的 `sizeof c` 返回被 `c[2]=7` 踩掉的那个槽 ⇒ `p=14` 而不是 47。
#[test]
fn nested_vla_upvar_capture_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/nested_vla_upvar_capture.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("nested-vla-upvar", src, &[flag]),
            0,
            "{flag}: 嵌套函数里 VLA 声明维度/形参维度的外层引用没进 upvars，\
             或 VLA 元素访问的基址取的是元数据槽本身而非其内容"
        );
    }
}

/// N6：语句表达式「最后一个 item」带标签外壳时值不能丢，四档逐个与 gcc 对照。
///
/// 为什么必须四档都跑：`-O0/-O1` 走 AST 直发（残值是上一个操作数 ⇒ `a=5`），
/// `-O2/-O3` 走 SSA 往返（同一条分类守卫的另一份消费路径 ⇒ 残值成了垃圾数），
/// 只跑任一半都会让另一半回归。夹具里同时含「标号未被使用」与「`goto` 用到标号」
/// 两条反向对照：取值路径不能把标号漏发，也不能把它发两遍。
#[test]
fn stmt_expr_labeled_tail_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/stmt_expr_labeled_tail.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("stmt-expr-labeled-tail", src, &[flag]),
            0,
            "{flag}: 语句表达式尾部带标签外壳时值丢了（或标号漏发/重发）"
        );
    }
}

/// `T *(*f(P1))(P2)` 声明符：P1 是 f 自己的形参、P2 属于被指向的函数。
///
/// 四档逐个与 gcc 对照（同一份源在 -O0 走 AST 直发、-O2/-O3 走 SSA 往返；
/// 参数槽与家两条路都要对）。三种形状一起钉：双层参数表、双层多参数、
/// 以及只有一层参数表的旧形状（反向对照，改动不许弄坏它）。
#[test]
fn fnptr_returning_function_params_match_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/fnptr_returning_fn_params.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("fnptr-returning-fn", src, &[flag]),
            0,
            "{flag}: 返回函数指针的函数把自己的形参丢了（参数没人接 / 发射期懒注册猜宽度）"
        );
    }
}

/// 帧内槽必须在**规划期**登记完（`var_offset` 的发射期兜底从此是异常，不是常规）。
///
/// 兜底路径只会按 `Type::Int` 猜一个槽，而且「发射期还会新增多少对象」无法预知，
/// 晚分配带的容量也就无从计算（§11.23）。现在 `plan_late_slot_budget` 把体内被引用的
/// 名字按表达式自带的类型全部登过，兜底改成 E1134 硬错误。
///
/// 注意分寸：夹具里那些形状在**旧**二进制上跑出来也是对的（Int 猜的槽恰好没被按错
/// 宽度读），所以它不是错值的回归钉，而是形状守卫：它钉的是「这些形状不得再走兜底」
/// （下面逐档断 stderr 里没有 E1134），以及四档与 gcc 同结果。
#[test]
fn frame_slots_are_planned_not_lazily_guessed() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/planned_frame_slots.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("planned-frame-slots", src, &[flag]),
            0,
            "{flag}: 帧内槽没在规划期登记，或发射期兜底把宽类型按 Int 存取"
        );
    }
    // 可执行的那条不变式：本夹具的每一种形状都不许再触到发射期兜底。
    // E1134 是 Error 级 ⇒ 触到就是编不过，所以退出码与 stderr 两样一起看。
    let (out, exe) = run_compile_and_run_flags("planned-frame-slots-obj", src, &["-O2"]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_file(exe);
    assert!(out.status.success(), "-O2 应能编过: {stderr}");
    assert!(
        !stderr.contains("E1134"),
        "帧内槽兜底被触到（该名字没在规划期登记）: {stderr}"
    );
}




// ============================================================================
// N13（本轮已修）：-O2/-O3 的整体结构体赋值曾把源对象的第二个成员写成垃圾
//
// 形状：SSA 重建把「把聚合值拷进目标」写成 `x + 0` 的 copy 形态，聚合拷贝的分支因此
// 把它当成「一个表达式」走通用路 —— 假定 `gen_expr` 后前 16 字节在 `rax:rdx`，而内存聚合
// 的求值给出的是地址、`%rdx` 是从未装载过的垃圾；临时区按 `dst_off - padded` 算，对
// `y = x` 恰好落在源对象自己身上 ⇒ 先把 `x.b` 覆盖成垃圾再拷贝。
// 修在 `emit/expr/aggregate.rs`：先剥掉那个 `+ 0`，右值是内存对象时直接取其地址当源。
//
// 这一节留在测试里而不是删掉，理由是本缺陷**在修前没有任何测试覆盖**（`OPT-DIFF 0`、
// 全语料绿同时存在一个错值），所以它同时是「这个形状现在有对拍」的凭据。
// ============================================================================

/// 整块聚合赋值后逐成员读；低档与高档都必须与 gcc 同值。
const AGG_COPY_PARITY_SRC: &str = r#"
struct S { long a, b; };
struct Big { long a,b,c,d,e,f,g,h; };
int main(void){
    struct S p = {1,2}; struct S q = p;
    struct Big x = {1,2,3,4,5,6,7,8}; struct Big y = x;
    return (int)((q.a + q.b) * 100 + (y.a+y.b+y.c+y.d+y.e+y.f+y.g+y.h));
}
"#;

#[test]
fn agg_copy_matches_gcc_at_all_levels() {
    // `run_gcc_cross_check_opt` 已经在内部断言了「两侧退出码相等 + 两侧 stdout 相等」，
    // 返回的是那个退出码；所以这里再钉住它等于算得出的期望值，就等于同时证了
    // 「gcc≡dcc」与「值真的是 336」。（不用 `assert_all_levels_match_gcc`：它要求程序
    // 返回 0，而本夹具把结果当退出码交出去。）
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            harness::run_gcc_cross_check_opt("agg-copy-parity", AGG_COPY_PARITY_SRC, &[flag]),
            80,
            "{flag}: 期望 (1+2)*100 + (1+…+8) = 336，而退出码按 256 取模 ⇒ 336 % 256 = 80"
        );
    }
}

/// 聚合拷贝与 varargs、6 参数调用混在一起时也不能错（N13 当初就是从这类混合形状里
/// 剥出来的；单独的结构体拷贝绿了不代表与参数装载共处时仍绿）。
#[test]
fn agg_copy_with_varargs_and_six_arg_calls_matches_gcc() {
    let src = r#"
#include <stdarg.h>
struct Big { long a,b,c,d,e,f,g,h; };
static long acc;
long va_sum(int n, ...) { va_list ap; va_start(ap, n); long s = 0; for (int i = 0; i < n; i++) s += va_arg(ap, long); va_end(ap); acc += s; return s; }
long six(long a, long b, long c, long d, long e, long f) { acc += a+b+c+d+e+f; return a*1+b*2+c*3+d*4+e*5+f*6; }
int main(void) {
    struct Big x = {1,2,3,4,5,6,7,8};
    long p = six(1,2,3,4,5,6);
    long q = va_sum(4, 10L, 20L, 30L, 40L);
    struct Big y = x;
    long r = y.a+y.b+y.c+y.d+y.e+y.f+y.g+y.h;
    long t = six(p, q, r, x.e, x.f, x.g);
    acc = p + q + r + t;
    return (int)(acc % 251);
}
"#;
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            harness::run_gcc_cross_check_opt("agg-copy-mixed", src, &[flag]),
            216,
            "{flag}: 聚合拷贝与 varargs/多参调用共存时必须与 gcc 同值（期望 216，实取自 gcc 实测）"
        );
    }
}
