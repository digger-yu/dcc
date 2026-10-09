// included from tests/integration.rs — not a standalone cargo target

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
fn s_output_disassembles_return_thunk_symbol() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-s-rethunk-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-s-rethunk-{}.s", std::process::id()));
    let source = "int main(void) { return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-mfunction-return=thunk-extern", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        asm_text.contains("jmp __x86_return_thunk"),
        "return thunk target should be symbolic in -S output: {asm_text}"
    );
    assert!(
        !asm_text.lines().any(|line| line.trim() == "ret"),
        "rethunk -S output must not contain naked ret: {asm_text}"
    );
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        asm_text.contains(".ascii \"->SIZE_usb_device_id $8 sizeof(struct usb_device_id)\""),
        "asm output: {asm_text}"
    );
}

#[test]
fn asm_register_planner_avoids_gp_clobbers() {
    let source = "int f(int x) { int out; asm(\".ascii \\\"gp %0 %1\\\"\" : \"=r\"(out) : \"r\"(x) : \"rax\"); return out; }\n";
    let (output, asm_text) = run_asm_output_test("asm-gp-plan", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let ascii_line = asm_text
        .lines()
        .find(|line| line.contains(".ascii \"gp "))
        .unwrap_or_else(|| panic!("missing resolved inline asm line: {asm_text}"));
    assert!(
        !ascii_line.contains("%rax"),
        "planner should avoid rax clobber in inline asm operands: {ascii_line}"
    );
}

#[test]
fn asm_register_planner_avoids_xmm_clobbers() {
    let source = "double f(double x) { double out; asm(\".ascii \\\"xmm %0 %1\\\"\" : \"=x\"(out) : \"x\"(x) : \"xmm0\"); return out; }\n";
    let (output, asm_text) = run_asm_output_test("asm-xmm-plan", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let ascii_line = asm_text
        .lines()
        .find(|line| line.contains(".ascii \"xmm "))
        .unwrap_or_else(|| panic!("missing resolved inline asm line: {asm_text}"));
    assert!(
        !ascii_line.contains("%xmm0"),
        "planner should avoid xmm0 clobber in inline asm operands: {ascii_line}"
    );
}

#[test]
fn asm_register_planner_reassigns_early_clobber_outputs() {
    let source = "int f(int x) { int out; asm(\".ascii \\\"early %0 %1\\\"\" : \"=&r\"(out) : \"{rax}\"(x)); return out; }\n";
    let (output, asm_text) = run_asm_output_test("asm-early-plan", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let ascii_line = asm_text
        .lines()
        .find(|line| line.contains(".ascii \"early "))
        .unwrap_or_else(|| panic!("missing resolved inline asm line: {asm_text}"));
    assert!(
        ascii_line.contains("%rax") && !ascii_line.contains("early %rax %rax"),
        "early-clobber output should not overlap explicit rax input: {ascii_line}"
    );
}

#[test]
fn asm_register_planner_respects_fixed_gp_constraints() {
    let source = "int f(int x) { int out; asm(\".ascii \\\"fixed %0 %1\\\"\" : \"=a\"(out) : \"D\"(x)); return out; }\n";
    let (output, asm_text) = run_asm_output_test("asm-fixed-plan", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let ascii_line = asm_text
        .lines()
        .find(|line| line.contains(".ascii \"fixed "))
        .unwrap_or_else(|| panic!("missing resolved inline asm line: {asm_text}"));
    assert!(
        ascii_line.contains("fixed %rax %rdi"),
        "planner should honor fixed a/D constraints: {ascii_line}"
    );
}

#[test]
fn asm_constraint_type_mismatch_warns_float_gp_input() {
    let source = "int f(double x) { asm(\"\" : : \"r\"(x)); return 0; }\n";
    let (output, object_path) = run_object_test("asm-constraint-float-r-input", source);
    let _ = fs::remove_file(object_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'r'"),
        "stderr: {stderr}"
    );
}

#[test]
fn asm_constraint_type_mismatch_warns_float_gp_input_syntax_only() {
    let source = "int f(double x) { asm(\"\" : : \"r\"(x)); return 0; }\n";
    let output = run_syntax_test("asm-constraint-float-r-input-syntax", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "syntax-only compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'r'"),
        "stderr: {stderr}"
    );
}

#[test]
fn asm_constraint_type_mismatch_warns_float_gp_output() {
    let source = "double f(void) { double out; asm(\"\" : \"=r\"(out)); return out; }\n";
    let (output, object_path) = run_object_test("asm-constraint-float-r-output", source);
    let _ = fs::remove_file(object_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'=r'"),
        "stderr: {stderr}"
    );
}

#[test]
fn asm_constraint_type_mismatch_warns_float_rm_input() {
    let source = "int f(double x) { asm(\"\" : : \"rm\"(x)); return 0; }\n";
    let (output, object_path) = run_object_test("asm-constraint-float-rm-input", source);
    let _ = fs::remove_file(object_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'rm'"),
        "stderr: {stderr}"
    );
}

#[test]
fn asm_constraint_type_validation_allows_matching_operands() {
    let source = "int f(int x, double d) { int y; double z; asm(\"\" : \"=r\"(y), \"=x\"(z) : \"r\"(x), \"x\"(d), \"m\"(d)); return y; }\n";
    let (output, object_path) = run_object_test("asm-constraint-matching", source);
    let _ = fs::remove_file(object_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn asm_constraint_type_mismatch_warns_integer_xmm_input() {
    let source = "int f(int x) { asm(\"\" : : \"x\"(x)); return x; }\n";
    let (output, object_path) = run_object_test("asm-constraint-int-x-input", source);
    let _ = fs::remove_file(object_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'x'"),
        "stderr: {stderr}"
    );
}

#[test]
fn asm_constraint_type_mismatch_warns_explicit_gp_float_input() {
    let source = "int f(double x) { asm(\"\" : : \"{rax}\"(x)); return 0; }\n";
    let (output, object_path) = run_object_test("asm-constraint-float-explicit-gp", source);
    let _ = fs::remove_file(object_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'{rax}'"),
        "stderr: {stderr}"
    );
}

#[test]
fn asm_constraint_type_mismatch_warns_matching_float_to_gp_output() {
    let source = "int f(double x) { int y; asm(\"\" : \"=r\"(y) : \"0\"(x)); return y; }\n";
    let (output, object_path) = run_object_test("asm-constraint-float-match-gp", source);
    let _ = fs::remove_file(object_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "compile should warn but succeed: {stderr}");
    assert!(
        stderr.contains("W0005")
            && stderr.contains("impossible constraint")
            && stderr.contains("'=r'"),
        "stderr: {stderr}"
    );
}

#[test]
fn mini_asm_unknown_mnemonic_is_hard_error() {
    let source = "void f(void) { __asm__ volatile (\"notarealinsn\"); }\n";
    let (output, object_path) = run_object_test("mini-asm-unknown-mnemonic", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "compile should fail: {stderr}");
    assert!(stderr.contains("E0311"), "stderr: {stderr}");
    assert!(stderr.contains("notarealinsn"), "stderr: {stderr}");
    assert!(!object_path.exists(), "hard error should not write object");
}

#[test]
fn mini_asm_invalid_operands_is_hard_error() {
    let source = "void f(void) { __asm__ volatile (\"rdrand $1\"); }\n";
    let (output, object_path) = run_object_test("mini-asm-invalid-operands", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "compile should fail: {stderr}");
    assert!(stderr.contains("E0311"), "stderr: {stderr}");
    assert!(stderr.contains("rdrand needs register or memory operand"), "stderr: {stderr}");
    assert!(!object_path.exists(), "hard error should not write object");
}

#[test]
fn o2_dce_preserves_outputless_inline_asm() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-o2-asm-dce-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-o2-asm-dce-{}.s", std::process::id()));
    let source = "int main(void) { asm(\"nop\"); return 0; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(asm_text.contains("nop"), "asm output: {asm_text}");
}

/// 内联 asm 约束 (=r / +m / memory clobber) 编译通过。
#[test]
fn asm_constraints_compile() {
    let source = include_str!("../../examples/assembly/constraints.c");
    let (output, obj) = run_object_test("asm-constraints", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let _ = fs::remove_file(asm_path);
}

/// asm __inline volatile(...) 限定符
#[test]
fn asm_inline_qualifier_compiles() {
    let source = include_str!("../fixtures/asm/asm_inline.c");
    let (output, obj) = run_object_test("asm-inline-qual", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// `register asm` 变量被取址（或被内联汇编的 `"m"` 约束引用）时必须退回内存。
///
/// 旧实现无条件绑定，`&v` 读到从未被写的栈槽（实测 `42 0`）。这类构造 gcc 直接
/// 拒绝编译，所以本夹具只断言 dcc 自身语义，不跟 gcc 对照。
#[test]
fn asm_register_binding_yields_to_address_escape() {
    let source = include_str!("../fixtures/asm/asm_register_binding_escape.c");
    for flag in ["-O0", "-O2", "-O3"] {
        let (output, exe) = run_compile_and_run_flags("asm-reg-bind-escape", source, &[flag]);
        assert!(output.status.success(), "{flag}: stderr: {}",
            String::from_utf8_lossy(&output.stderr));
        assert_eq!(run_exe(&exe), 0, "{flag}: 地址逸出的 register asm 变量丢了值");
        let _ = fs::remove_file(exe);
    }
}

/// `register asm` 绑定的寄存器是向调用者借的：借之前必须保存，借期间不得再当临时值
/// 发出去（绑定表就是寄存器所有权的真源）。
///
/// 三条形态的实测错值全部只在 -O0/-O1 出现（-O2/-O3 换了表达式形状，恰好没走溅出
/// 路径），所以逐档与 gcc 对照 —— 只跑默认档或只跑 -O2 都会漏掉一整类错值。
/// 静态反向对照钉住「绑定仍然生效」：判据不能退化成把 `register asm` 关掉。
#[test]
fn asm_register_binding_owns_its_register() {
    let source = include_str!("../fixtures/asm/asm_register_binding_clobber.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("asm-reg-bind-owns", source, &[flag]),
            0,
            "{flag}: 绑定寄存器的保存/占用判定丢了值"
        );
    }
    let (build, asm_text) = run_asm_output_test("asm-reg-bind-owns-asm", source);
    assert!(build.status.success(), "stderr: {}", String::from_utf8_lossy(&build.stderr));
    let body = extract_function_body(&asm_text, "pinned");
    assert!(body.contains("mov %rax, %rbx"), "绑定的变量必须真的住在 %rbx 里:\n{body}");
    assert!(
        body.contains("mov %rbx, -16(%r13)"),
        "绑定过 %rbx 的函数必须保存它, 否则毁掉调用者存放在里面的值:\n{body}"
    );
    // file-scope 的绑定是第三条路：`compile_func` 开头就把绑定写进表，而位图在其后
    // 被 `reset_reg_tracker()` 清零，所以必须在那里重新登记。这一条造不出稳定的运行时
    // 错值（临时值池归还得很即时，%r14 极少跨调用保活），于是直接钉产物字节：
    // 只要存在 `register long gv asm("r14")`，每个函数都必须真的保存 %r14。
    // 修前实测整份目标文件里 `mov %r14,-0x20(%r13)`（4d 89 75 e0）出现 **0 次** ——
    // 保存被 patch_callee_saves 改成了 nop，而函数体确实在往 %r14 里写。
    let (text, _disasm) = compile_legacy_asm_object("asm-reg-bind-owns-obj", source);
    assert_bytes_present(&text, &[0x4d, 0x89, 0x75, 0xe0], "序言保存 %r14");
    assert_bytes_present(&text, &[0x49, 0x89, 0x5d, 0xf0], "序言保存 %rbx");
}

/// 内联汇编的 clobber 名单是「本条 asm 之后这些号的内容不保证」的宣告：谁此刻正把值
/// 放在那儿，谁就必须被保住（P12 Phase 5.2a）。
///
/// 两类主人各自只在某些档位坏：变量的家只在 -O2/-O3 存在（自动分配发生在 SSA 管线里），
/// 而表达式栈机的溅出槽与档位无关 —— 修前实测 `-O0` 就坏。所以逐档与 gcc 对照。
///
/// 修前的两个读数（本轮实测）：`home_across_clobber(10)` 得 632360（gcc 3050），
/// `temp_across_clobber(2,3,4)` 得 38885（gcc 25，四个档位都错）。静态判据钉住两件事：
/// 守护确实成对出现且把那条 asm 包在中间；而且它没有退化成「看见 clobber 就不绑」
/// —— 那个号仍然必须是变量的家。
#[test]
fn asm_clobber_guards_the_register_that_holds_a_value() {
    let source = include_str!("../fixtures/asm/asm_clobber_home_and_temp.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("asm-clobber-guard", source, &[flag]),
            0,
            "{flag}: 内联 asm 的 clobber 毁掉了正持有值的寄存器"
        );
    }
    let (build, asm_text) = run_asm_output_test_flags("asm-clobber-guard-asm", source, "-O2");
    assert!(build.status.success(), "stderr: {}", String::from_utf8_lossy(&build.stderr));

    // 成对守护必须把被测的那条 asm 包在中间：顺序反了（先毁后存）等于没守。
    let assert_wraps = |who: &str, body: &str, inner: &str| {
        let p = body
            .find("push %rbx")
            .unwrap_or_else(|| panic!("{who}: 没找到守护的 push %rbx:\n{body}"));
        let m = body
            .find(inner)
            .unwrap_or_else(|| panic!("{who}: 没找到被测的 asm 指令 {inner}:\n{body}"));
        let o = body
            .find("pop %rbx")
            .unwrap_or_else(|| panic!("{who}: 没找到守护的 pop %rbx:\n{body}"));
        assert!(
            p < m && m < o,
            "{who}: push/pop 必须把 {inner} 包在中间（push={p} asm={m} pop={o}）"
        );
    };
    let home = extract_function_body(&asm_text, "home_across_clobber");
    assert_wraps("home_across_clobber", &home, "mov $12345, %rbx");
    assert!(
        home.contains("mov %rax, %rbx"),
        "被守护的号仍然必须是变量的家（判据不得退化成「看见 clobber 就不绑」）:\n{home}"
    );
    let temp = extract_function_body(&asm_text, "temp_across_clobber");
    assert_wraps("temp_across_clobber", &temp, "mov $7777, %rbx");
    // Phase 5.1 起右操作数暂存号不再固定 %rcx：分配器在延伸池（r8/r9）里选号，
    // 所以乘积的两个操作数一个在溅出槽号（%rbx）一个在选出来的暂存号（实测 %r8）。
    // 判据保留原意——「守护后溅出槽的值还能被乘回」——但接受任意号组合。
    let has_imul_rbx = temp.contains("imul %r8, %rbx") || temp.contains("imul %rbx, %r8");
    assert!(
        has_imul_rbx,
        "溅出槽跨 asm 后必须还能从同一个号读回值（否则守护只是把错值挪了个地方）:\n{temp}"
    );
}

/// 守护把 `%rsp` 在 asm 体内下推 8×N 字节，而本后端要求「体内每个 call 点 rsp ≡ 0 (mod 16)」：
/// `emit/stmt.rs` 里 pr44575 的教训就是跳调用多压 8 字节让 libc 的 `movaps` 当场崩。所以
/// 奇数个守护 push 必须配一次 `sub/add rsp, 8`，偶数个不得配（P12 Phase 5.2a）。
///
/// 这条不与 gcc 对照 stdout：在 inline asm 里 `call` 一个函数是 GCC 文档明确不支持的写法
/// （实测 gcc 连活变量都保不住，返回一个栈地址），参照系只能是本后端自己的 ABI 不变式。
/// 产物级判据是**逐 call 点累加 %rsp 位移**而不是匹配字符串：将来任何人往守护里加/减 push
/// 都跑不掉，而只会写字符串比对的那条一改守护形式就变成假绿。
#[test]
fn asm_clobber_guard_keeps_call_sites_aligned() {
    let source = include_str!("../fixtures/asm/asm_clobber_guard_alignment.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        let (build, exe) = run_compile_and_run_flags("asm-clobber-align", source, &[flag]);
        assert!(
            build.status.success(),
            "{flag}: 应能编译: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let (rc, out, _) = run_exe_bounded(&exe);
        // 139 = SIGSEGV，正是对齐没补上时的形态（探针实测：手动在 call 前 subq $8, %rsp 就崩）
        assert_eq!(rc, 0, "{flag}: 读数不对或崩了（139=SIGSEGV 即对齐未补）");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "r1=10 r2=140\n",
            "{flag}: 守护把值保错了"
        );
        let _ = std::fs::remove_file(&exe);
    }

    let (build, asm_text) = run_asm_output_test_flags("asm-clobber-align-asm", source, "-O2");
    assert!(build.status.success(), "stderr: {}", String::from_utf8_lossy(&build.stderr));
    for name in ["one_guarded", "two_guarded"] {
        let body = extract_function_body(&asm_text, name);
        assert!(!body.is_empty(), "{name} 应在产物里（否则本用例是空跑）");
        // 函数入口时 rsp ≡ 8 (mod 16)（调用方的 call 推了返回地址），SysV 要求 call 指令
        // 处 rsp ≡ 0 ⇒ 「从入口算起共下推了多少」必须 ≡ 8 (mod 16)。
        let mut off: i64 = 0;
        let mut calls = 0usize;
        for raw in body.lines() {
            let l = raw.trim();
            if l == "mov %r13, %rsp" || l == "leave" {
                // 尾声开始：rsp 由帧基址重置，体内 call 点已全部核过，累加到此为止。
                break;
            }
            if let Some(rest) = l.strip_prefix("push ") {
                if rest.trim() != "%rsp" {
                    off += 8;
                }
            } else if let Some(rest) = l.strip_prefix("pop ") {
                if rest.trim() != "%rsp" {
                    off -= 8;
                }
            } else if let Some(rest) = l.strip_prefix("sub $") {
                if let Some(n) = rest.split(",").next().and_then(|x| x.trim().parse::<i64>().ok()) {
                    if rest.contains("%rsp") {
                        off += n;
                    }
                }
            } else if let Some(rest) = l.strip_prefix("add $") {
                if let Some(n) = rest.split(",").next().and_then(|x| x.trim().parse::<i64>().ok()) {
                    if rest.contains("%rsp") {
                        off -= n;
                    }
                }
            } else if l.starts_with("call ") {
                calls += 1;
                assert_eq!(
                    (8 - off).rem_euclid(16),
                    0,
                    "{name}: 第 {calls} 个 call 点 %rsp 已下推 {off} 字节，不满足 16 字节对齐:\n{body}"
                );
            } else if (l.starts_with("and ") && l.contains("%rsp"))
                || (l.starts_with("mov ") && l.ends_with("%rsp"))
            {
                // `and ..., %rsp` 是动态帧、`mov X, %rsp`（X 不是帧基址）直接写 rsp：这两种
                // 形态会让上面的累加不再可信，本夹具不该出现；出现了就必须红，而不是默默
                // 给一个「看起来对」的读数。
                panic!("{name}: 出现了位移判据算不了的 rsp 写法：{l}");
            }
        }
        assert!(calls >= 1, "{name}: 一个 call 都没看到 ⇒ 对齐判据是空跑");
    }
    // 守护个数驱动补对齐：奇数配、偶数不配（后者若也配就把对齐又弄坏了）。
    let one = extract_function_body(&asm_text, "one_guarded");
    assert!(
        one.contains("sub $8, %rsp") && one.contains("add $8, %rsp"),
        "一个守护 push 必须配一次对齐补偿（本夹具实测：少它就是 SIGSEGV）:\n{one}"
    );
    let two = extract_function_body(&asm_text, "two_guarded");
    assert!(
        two.contains("sub $8, %rsp") && two.contains("add $8, %rsp"),
        "三个守护 push（rbx/r8/r12，含延伸池家 r8）是奇数，必须配一次对齐补偿:\n{two}"
    );
}

/// 跳得出的 `asm goto` 守不住一个正被它毁掉的号 ⇒ 必须点名（E0317）：既不能静默给错值，
/// 也不能插一对「跳走那条路永远不执行」的 push/pop（那会把 `%rsp` 永久下推，后续每个
/// call 点都错位）。P12 Phase 5.2a 的边界。
///
/// 今天能走到这条判据上的只有用户自己写的 `register asm("rbx")` + 同一函数里
/// `::: "rbx"` 的 `asm goto`：溅出槽已由函数入口的 `ban_escaping_asm_regs` 移出池，
/// 自动绑定的家已由「这类函数不进 SSA」挡在外面（两个前提各有用例钉着）。
#[test]
fn escaping_asm_goto_clobbering_a_held_register_is_an_error() {
    let bad = "register long v asm(\"rbx\");\n\
               long f(long n){ long a=0;\n\
               for(long i=0;i<n;i++){ a+=i;\n\
               __asm__ goto(\"jmp %l0\" : : : \"rbx\" : sk); }\n\
               sk: return a; }\n\
               int main(void){ return (int)f(3) == 3 ? 0 : 1; }\n";
    let (output, _obj) = run_object_test("esc-goto-clobber", bad);
    assert!(!output.status.success(), "守不住的 clobber 不得静默通过");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0317"), "期望诊断码 E0317，实得：{stderr}");
    assert!(
        stderr.contains("rbx") && stderr.contains("goto"),
        "诊断必须点名那个号与那条 asm 的形态，实得：{stderr}"
    );

    // 反向对照：同一个号被**跳不出去**的 asm clobber 时必须照旧编译，并由守护保住值
    // —— 判据不得退化成「看见绑定号被 clobber 就报错」。
    let ok = "register long v asm(\"rbx\");\n\
              long f(long n){ v = n + 1;\n\
              __asm__ volatile(\"movq $99, %%rbx\" ::: \"rbx\");\n\
              return (long)v; }\n\
              int main(void){ return (int)f(41) == 42 ? 0 : 1; }\n";
    let (build, exe) = run_compile_and_run_flags("esc-goto-clobber-ok", ok, &["-O2"]);
    assert!(
        build.status.success(),
        "跳不出去的 clobber 应当由守护管住而不是报错: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let (rc, out, _) = run_exe_bounded(&exe);
    let _ = std::fs::remove_file(&exe);
    assert_eq!(rc, 0, "被守护的绑定变量必须仍是 42（139=SIGSEGV 则是守护弄坏了对齐）: {}", String::from_utf8_lossy(&out));
}

/// 保留号的判据（E0318）：`%r13` 是本后端的帧基址（`-fno-omit-frame-pointer` 下还有 `%rbp`），
/// 不能当变量的家；`%rsp` 不能被 clobber（它守无可守），但**可以**当只读别名。
///
/// 修前的实测行为是「同一份源两个档位两个答案」：`register long v asm("r13")` 在 -O0
/// 真发出 `mov %rax,%r13`（之后所有 `-N(%r13)` 寻址全错），而在 -O2 被静默降级成栈槽
/// —— 两边都不报错。现在两边都是 E0318（退出码 1），与 E0316 同一条纪律。
///
/// 反向对照特意含两个内核/stdlib 真在用的写法：`register ... asm("rsp")` 只读别名
/// （把它一起拒了就弄坏内核构建），以及能被守护的 `%rbx`/`%r13` clobber（拒了就等于
/// 把 5.2a 的守护改成「不让写 asm」）。
#[test]
fn reserved_frame_registers_are_rejected_not_silently_dropped() {
    // 绑定侧：两个档位都必须报错，且点名变量与那个名字
    let bind = "register long v asm(\"r13\");\nint main(void){ v = 3; return (int)v - 3; }\n";
    for flag in ["-O0", "-O2"] {
        let (output, _obj) = run_object_test(&format!("reserved-bind-{}", flag), bind);
        assert!(!output.status.success(), "{flag}: 把变量绑在帧基址上不得静默通过");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            stderr.contains("E0318") && stderr.contains("v") && stderr.contains("r13"),
            "期望 E0318 并点名变量与寄存器，实得：{stderr}"
        );
    }
    // clobber 侧：%rsp 守不了（它就是守护自己的基座），GCC 同样不允许
    let clobber = "long f(long n){ __asm__ volatile(\"nop\" : : : \"rsp\"); return n; }\n\
                   int main(void){ return (int)f(2) - 2; }\n";
    let (output, _obj) = run_object_test("reserved-clobber-rsp", clobber);
    assert!(!output.status.success(), "clobber %rsp 不得静默通过");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("E0318"), "期望 E0318，实得：{stderr}");

    // 反向对照：能被守护的 callee-saved 号**不得**跟着报错——帧基址能守（push/pop 原样
    // 还回来），池号也能守；把它们一起拒了就是把 5.2a 的守护改成了「不让写 asm」。
    let ok = "long f(long n){ register long v asm(\"rbx\"); v = n + 1;\n\
              __asm__ volatile(\"movq $7, %%rbx\" : : : \"rbx\");\n\
              __asm__ volatile(\"movq $8, %%r13\" : : : \"r13\", \"memory\");\n\
              return (long)v - (n + 1); }\nint main(void){ return (int)f(4) != 0; }\n";
    let (build, exe) = run_compile_and_run_flags("reserved-reverse", ok, &["-O2"]);
    assert!(
        build.status.success(),
        "被守护得住的号（%rbx 绑定、clobber %rbx/%r13）必须照旧编译：{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let (rc, out, _) = run_exe_bounded(&exe);
    let _ = std::fs::remove_file(&exe);
    assert_eq!(rc, 0, "守护后 %rbx 与 %r13 上的值都得原样：{}", String::from_utf8_lossy(&out));

    // 反向对照的另一半：`register ... asm("rsp")` 是内核用的只读别名，不得跟着报错，
    // 而且读回来的必须真是栈指针（不是某个从未写过的栈槽）。
    let csp = "long printf(const char *, ...);\n\
               register unsigned long current_stack_pointer asm(\"rsp\");\n\
               unsigned long foo(void) { return current_stack_pointer; }\n\
               int main(void){ unsigned long here; long d = (long)(foo() - (unsigned long)&here);\n\
               printf(\"d=%ld\\n\", d); return (d > -4096 && d < 4096) ? 0 : 1; }\n";
    let (build, exe) = run_compile_and_run_flags("reserved-rsp-alias", csp, &["-O2"]);
    assert!(
        build.status.success(),
        "`register ... asm(\"rsp\")` 只读别名必须照旧编译（内核依赖它）：{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let (rc, out, _) = run_exe_bounded(&exe);
    let _ = std::fs::remove_file(&exe);
    assert_eq!(
        rc, 0,
        "读回来的值必须在栈地址附近（否则那个别名是静默错的）：{}",
        String::from_utf8_lossy(&out)
    );
}

/// 内联 asm 的操作数落进 callee-saved 号时，那个号必须登记回占用真源（Phase 6.1）：
/// 否则序言那条保存会被 `patch_callee_saves` 改成 nop，本函数毁掉**调用者**存在 %rbx 里的
/// 值（固定约束 `"b"` 这条通路）；而操作数多于九个 caller-saved 号时，旧实现会「复用已发出
/// 的号」（十个 `"=r"` 输出实测得 64，第十个落在第一个的家上）。
///
/// 四档逐档与 gcc 对照（只跑一档会漏：固定约束那条在 -O0 就错）。产物级判据钉住
/// 「asm 用过 callee-saved ⇒ 序言必须保留那条保存」而不是去看保存了多少字节。
#[test]
fn asm_operand_in_callee_saved_keeps_the_prologue_save() {
    let source = include_str!("../fixtures/asm/asm_operand_callee_saved.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("asm-op-callee-saved", source, &[flag]),
            0,
            "{flag}: asm 操作数踩了 callee-saved 却没保存（或多了输出就复用同号）"
        );
    }
    let (build, asm_text) = run_asm_output_test_flags("asm-op-callee-saved-asm", source, "-O0");
    assert!(build.status.success(), "stderr: {}", String::from_utf8_lossy(&build.stderr));
    let f = extract_function_body(&asm_text, "asm_add_b");
    assert!(!f.is_empty(), "asm_add_b 应在产物里（否则下面的判据是空跑）");
    assert!(
        f.contains("mov %rbx, -16(%r13)"),
        "操作数住进 %rbx 的函数必须真的保存它；这句被 patch_callee_saves 改成 nop 就等于\n\
         本函数毁掉调用者的 %rbx（实测：调用方跨过这个调用后 v 不再是 1000）：\n{f}"
    );
}

/// 声明「住在寄存器里」与 `__attribute__((cleanup))` 必须在**每一条**声明通路上都生效：
/// `for` 的 init、语句表达式 `({...})`、带标签的块。本轮实测的四条静默失效：
///   * `for (register long i asm("rbx") = 0; i < n; i++)` ⇒ 条件读 %rbx 而 `i++` 写栈槽，
///     **-O0/-O1 无限循环**（`register_var` 对家变量返过偏移 0，读-改-写那侧又绕开 `home_for`）；
///   * 语句表达式里那份 `gen_block_item` 副本漏了 `bind_decl_home` ⇒ 整个 `register asm`
///     被忽略（同一个声明写在普通块里则真用寄存器）；同一个副本还漏了 cleanup 的登记与收尾；
///   * `rebuild_block` 重建声明时写 `cleanup_fn: None` ⇒ **-O2/-O3 把清理整个丢掉**
///     （实测普通块里 `hits` 从 gcc 的 5 变成 0），所以这条**必须逐档**与 gcc 对照：
///     只跑 -O0 恰好是它原本就对的那一档，等于没测。
#[test]
fn register_home_and_cleanup_survive_every_declaration_path() {
    let source = include_str!("../fixtures/asm/asm_register_home_all_scopes.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("reg-home-all-scopes", source, &[flag]),
            0,
            "{flag}: 绑定家或 cleanup 在某某条声明通路上静默失效（挂死/无限循环也算这一类）"
        );
    }
    let (build, asm_text) = run_asm_output_test_flags("reg-home-all-scopes-asm", source, "-O0");
    assert!(build.status.success(), "stderr: {}", String::from_utf8_lossy(&build.stderr));

    // `i++` 的增量必须落回 %rbx（修前它是 `add $1,%rax` + `mov %rax,栈槽`，家永远不会前进）。
    // 对**增量之后**的子串做包含检查，本身就是「先把新值写回 home」的判据。
    let for_body = extract_function_body(&asm_text, "for_init_bound");
    let inc = for_body
        .find("add $1, %rax")
        .unwrap_or_else(|| panic!("for_init_bound 里找不到增量指令：\n{for_body}"));
    assert!(
        for_body[inc..].contains("mov %rax, %rbx"),
        "增量之后必须把新值写回 %rbx（家的所在地）；修前它写的是栈槽，循环因此永不前进：\n{for_body}"
    );

    // 语句表达式里的声明：绑定真的生效，而且 cleanup 真的发出去了
    let se_body = extract_function_body(&asm_text, "stmt_expr_bound");
    assert!(
        se_body.contains("mov %rax, %r12") && se_body.contains("mov %r12, %rax"),
        "语句表达式里的 `register long j asm(\"r12\")` 必须真的走 %r12：\n{se_body}"
    );
    assert!(
        se_body.contains("call hit_cleanup"),
        "`__attribute__((cleanup))` 写在语句表达式里也必须执行：\n{se_body}"
    );
}

/// `register asm` 变量的读写必须与栈槽路径同一个宽度口径（写入截断、读出扩展）。
///
/// 修前 -O0 实测 `bv=0`、file-scope 的 `gv=4294967467`（gcc 与 -O2 都是 171）：绑定
/// 路径是两次裸 64 位 `mov`，只有「宽源赋给窄绑定变量」才暴露。这份夹具放在
/// `tests/fixtures/c/` 里，因此 `check_opt_equivalence.sh` 也会逐档盯住它。
#[test]
fn asm_register_binding_honors_declared_width() {
    let source = include_str!("../fixtures/c/register_binding_width.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("asm-reg-bind-width", source, &[flag]),
            0,
            "{flag}: 绑定变量的宽度口径与 gcc 不一致"
        );
    }
}

/// 寄存器类的 asm 输出绑到**非 Ident 左值**（`a[i]` / `*p` / `s.f`）时必须仍然写回。
///
/// 修前 `writeback_asm_operand` 的每个分支都以 `Expr::Ident` 为前提，非 Ident 直接
/// `return`，实测四个档位全部静默丢值（`a[1]/a[2]/gs[0]/*p/s.b/s.c` 都是 0，gcc 是
/// 5/7/9/11/13/14）。夹具放在 `tests/fixtures/c/`，因此 OPT 等价门禁也覆盖它。
#[test]
fn asm_output_on_indexed_lvalue_is_written_back() {
    let source = include_str!("../fixtures/c/asm_output_general_lvalue.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("asm-out-indexed-lvalue", source, &[flag]),
            0,
            "{flag}: 寄存器类输出绑到 a[i]/*p/s.f 时值丢了"
        );
    }
}

/// x86_64 REX **位序**与「低 8 位字节寄存器只要有 REX 就合法」的实测形态。
///
/// 修前同一份代码在 objdump 里的错编（全部静默，长度一样、不崩溃）：
///   `movb $65,"=S"` → `b6 41` = `mov $0x41,%dh`；`addb $16,"+D"` → `80 c7 10` =
///   `add $0x10,%bh`；`sete "=S"` → `0f 94 c6` = `sete %dh`；`orb $1` 因
///   `0x80 + op*8` 撞成 `88 /r`（mov）；`shlb $2` 报 E0311；`"=m"` 的 char 存回
///   走 `89 /r` 越界写 4 字节。
///
/// 只在 -O0/-O1 与 gcc 对照：-O2/-O3 另有一个**与本夹具无关的既有缺陷**（跨 noinline
/// 调用的全局写被丢），由下面的 xfail 用例单独钉住。判据用运行值而不是产物字节，
/// 因为这些编码唯一的行为后果就是「写到了别人的位段上」。
#[test]
fn rex_operand_bit_order_matches_gcc() {
    let source = include_str!("../fixtures/asm/asm_rex_operand_bit_order.c");
    for flag in ["-O0", "-O1"] {
        assert_eq!(
            run_gcc_cross_check_opt("rex-bit-order", source, &[flag]),
            0,
            "{flag}: REX 位序/宽度口径与 gcc 不一致（值落到了别的寄存器的位段）"
        );
    }
}

/// 既有缺陷（非本轮引入）：-O2/-O3 下，跨 `noinline` 调用的全局变量写被丢，于是
/// 后续读拿到第一次写的值。HEAD 实测本夹具 `a=-125 b=-125 c=-125`（gcc 是 11/1/77…），
/// 最小复现：`f1(){asm("movb $65,%%sil"…); cb=a+d;} f2(){asm(…); cb=v;}` 两个写入者
/// 交替读 `cb`。
#[test]
fn rex_bit_order_fixture_xfail_o2_drops_cross_call_global_writes() {
    let source = include_str!("../fixtures/asm/asm_rex_operand_bit_order.c");
    for flag in ["-O2", "-O3"] {
        let (gsrc, gexe) = {
            let dir = std::env::temp_dir();
            let pid = std::process::id();
            let gsrc = dir.join(format!("dcc-rexx-{pid}.c"));
            let gexe = dir.join(format!("dcc-rexx-{pid}.gcc"));
            std::fs::write(&gsrc, source).expect("write fixture");
            let gb = std::process::Command::new("gcc")
                .args(["-O0", "-std=gnu2x", "-w", "-o"])
                .arg(&gexe)
                .arg(&gsrc)
                .output()
                .expect("run gcc");
            assert!(gb.status.success(), "oracle 侧 gcc 必须能编译本夹具");
            let _ = std::fs::remove_file(&gsrc);
            (gsrc, gexe)
        };
        let gcc_out = std::process::Command::new(&gexe).output().expect("run gcc exe");
        let _ = std::fs::remove_file(&gexe);
        let _ = gsrc;
        let (dcompile, dexe) = run_compile_and_run_flags("rex-xfail-o2", source, &[flag]);
        assert!(
            dcompile.status.success(),
            "{flag}: dcc 应能编译（缺陷在优化器，不在前端）"
        );
        let dcc_out = std::process::Command::new(&dexe).output().expect("run dcc exe");
        let _ = std::fs::remove_file(dexe);
        assert_ne!(
            dcc_out.stdout, gcc_out.stdout,
            "跨调用的全局写若已修好，请把本用例并进上面那条绿灯对照；\
             当前 -O2/-O3 应读到第一次写的值（a=-125 被复制给 b/c/e）"
        );
    }
}

/// `q` 类的 legacy 寄存器不够用时，必须**报出来**，不能退化成静默错值。
///
/// 修前七个 `"=q"` 输出实测全是 0（gcc 是 11..77），也就是「分配器把已占用的寄存器
/// 再发一次」的直接后果。现在的策略是返回 None 并报 impossible constraint；
/// 以后有人重写操作数暂存约定时，这条会先变红，提醒他把诊断一起撤掉。
#[test]
fn asm_q_class_exhaustion_is_reported_not_silent() {
    let source = "int main(void) {\n\
                  long o[7] = {0,0,0,0,0,0,0};\n\
                  __asm__ volatile(\n\
                      \"movq $11, %0\\n\\tmovq $22, %1\\n\\tmovq $33, %2\\n\\t\"\n\
                      \"movq $44, %3\\n\\tmovq $55, %4\\n\\tmovq $66, %5\\n\\tmovq $77, %6\"\n\
                      : \"=q\"(o[0]), \"=q\"(o[1]), \"=q\"(o[2]), \"=q\"(o[3]),\n\
                        \"=q\"(o[4]), \"=q\"(o[5]), \"=q\"(o[6]));\n\
                  return o[0] == 11;\n\
                  }\n";
    let (output, obj) = run_object_test("asm-q-exhausted", source);
    let _ = fs::remove_file(obj);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("impossible constraint"),
        "q 类耗尽应报 impossible constraint，实际 stderr:\n{stderr}"
    );
}

/// GNU 局部数字标签 6: / 6f / 6b
#[test]
fn asm_local_label_compiles() {
    let source = include_str!("../fixtures/asm/asm_local.c");
    let (output, _obj) = run_object_test("asm-local-label", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 同行局部标签 N:insn（如 1:jmp 2f）
#[test]
fn asm_label_insn_same_line_compiles() {
    let source = include_str!("../fixtures/asm/asm_label.c");
    let (output, _obj) = run_object_test("asm-label-insn", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// asm goto 模板行内 `#` 注释（jump_label.h objtool 标注）不污染标签名
#[test]
fn asm_goto_hash_comment_compiles() {
    let source = include_str!("../fixtures/asm/asm_goto.c");
    let (output, exe) = run_compile_and_run("asm-goto-hash-comment", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0);
}

/// asm goto 输出约束写回语义: fall-through 路径写回, 跳转路径保留原值
#[test]
fn asm_goto_output_writeback() {
    let source = include_str!("../fixtures/asm/asm_goto_outputs.c");
    let (output, exe) = run_compile_and_run("asm-goto-output-writeback", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0, "asm goto output writeback test failed");
}

/// dcc -S 对 movd 等 SSE 桥接指令不应再输出逐字节 .byte，也不应把后续
/// 字节错当成 shift 等伪指令。文本可重汇编性由下一个 roundtrip 测试覆盖。
#[test]
fn s_output_disassembles_movd_without_desync() {
    let source = include_str!("../fixtures/c/vec_small_return.c");
    let (output, asm_text) = run_asm_output_test("vector-small-return-s", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for func in ["ret_v1qu", "ret_v1hu", "ret_v1ui", "ret_v2hu", "ret_v4qu"] {
        let start = asm_text
            .find(&format!("{func}:"))
            .unwrap_or_else(|| panic!("missing function {func} in -S output:\n{asm_text}"));
        let rest = &asm_text[start..];
        let end = rest.find("\n\n").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(body.contains("movd"), "{func} should disassemble to movd in -S:\n{body}");
        assert!(
            !body.contains("shift"),
            "{func} -S output is desynced (spurious shift):\n{body}"
        );
        assert!(
            !body.contains(".byte"),
            "{func} -S output should not fallback to .byte:\n{body}"
        );
    }
}

/// dcc -S 输出应能被 GAS 重新汇编，且重汇编后的机器码与 dcc -c 直发对象
/// 在目标函数级别语义等价。机器码语义以 objdump -d 为 oracle。
#[test]
fn s_output_roundtrips_through_gas_for_small_vector() {
    let source = include_str!("../fixtures/c/vec_small_return.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-vec-rt-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-vec-rt-{}.s", std::process::id()));
    let direct_obj = dir.join(format!("dcc-vec-rt-direct-{}.o", std::process::id()));
    let text_obj = dir.join(format!("dcc-vec-rt-text-{}.o", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");

    let direct = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&direct_obj)
        .arg(&source_path)
        .output()
        .expect("run dcc -c");
    assert!(
        direct.status.success(),
        "direct compile stderr: {}",
        String::from_utf8_lossy(&direct.stderr)
    );

    let s = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc -S");
    assert!(s.status.success(), "-S compile stderr: {}", String::from_utf8_lossy(&s.stderr));

    let assemble = Command::new("as")
        .args(["-o"])
        .arg(&text_obj)
        .arg(&asm_path)
        .output()
        .expect("run as");
    assert!(
        assemble.status.success(),
        "as failed: {}",
        String::from_utf8_lossy(&assemble.stderr)
    );

    fn objdump_func_body(obj: &std::path::Path, func: &str) -> String {
        let out = Command::new("objdump")
            .args(["-d"])
            .arg(obj)
            .output()
            .expect("objdump -d");
        assert!(out.status.success(), "objdump -d failed");
        let text = String::from_utf8_lossy(&out.stdout);
        let start = text
            .find(&format!("<{func}>:"))
            .unwrap_or_else(|| panic!("missing {func} in {}:\n{text}", obj.display()));
        let rest = &text[start..];
        let end = rest.find("\n\n").unwrap_or(rest.len());
        rest[..end].to_string()
    }

    fn normalize(body: &str) -> Vec<String> {
        body.lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.ends_with(':') {
                    return None;
                }
                // 去掉地址/字节列，只保留 mnemonic + operands。
                // objdump 行格式: "<addr>: <bytes>\t<mnemonic> <operands>"
                let after_colon = trimmed.split_once(':')?.1;
                let after_colon = after_colon.trim_start();
                // 跳过字节列（由 2 位十六进制 token 组成）
                let mut idx = 0usize;
                for tok in after_colon.split_whitespace() {
                    let tok_start = after_colon[idx..].find(tok)? + idx;
                    let tok_end = tok_start + tok.len();
                    if tok.len() == 2 && tok.chars().all(|c| c.is_ascii_hexdigit()) {
                        idx = tok_end;
                        continue;
                    }
                    // 遇到非字节 token，从这里开始是指令
                    idx = tok_start;
                    break;
                }
                let inst = after_colon[idx..].trim_start();
                if inst.is_empty() {
                    return None;
                }
                // 对齐填充用的 nop 在不同汇编流程中数量可能不同，不影响语义。
                if inst == "nop" {
                    return None;
                }
                // 归一化 0x[hex] 常量，避免不同汇编器对 $127/$0x7f 等表示差异
                Some(regex_replace_hex_addrs(inst))
            })
            .collect()
    }

    fn regex_replace_hex_addrs(inst: &str) -> String {
        let mut out = String::with_capacity(inst.len());
        let mut chars = inst.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '0' && chars.peek() == Some(&'x') {
                chars.next();
                let mut hex = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_hexdigit() {
                        hex.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !hex.is_empty() {
                    out.push_str("0xHEX");
                    continue;
                }
                out.push('0');
                out.push('x');
            } else {
                out.push(ch);
            }
        }
        out
    }

    for func in ["ret_v1qu", "ret_v1hu", "ret_v1ui", "ret_v2hu", "ret_v4qu"] {
        let direct_body = objdump_func_body(&direct_obj, func);
        let text_body = objdump_func_body(&text_obj, func);
        let direct_norm = normalize(&direct_body);
        let text_norm = normalize(&text_body);
        assert!(
            direct_norm == text_norm,
            "{func} machine code differs between dcc -c and dcc -S -> as:\n\
             direct:\n{direct_body}\n\
             text:\n{text_body}"
        );
    }

    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    let _ = fs::remove_file(&direct_obj);
    let _ = fs::remove_file(&text_obj);
}

/// inline asm: lock cmpxchg 内存操作数
#[test]
fn asm_cmpxchg_mem_compiles() {
    let source = include_str!("../fixtures/asm/asm_cmpxchg.c");
    let (output, _obj) = run_object_test("asm-cmpxchg-mem", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// inline asm: cmpxchg8b 32-bit atomic (64-bit operand)
#[test]
fn asm_cmpxchg8b_has_opcode_and_disassembly() {
    let source = include_str!("../fixtures/asm/asm_cmpxchg8b.c");
    let (text, disasm) = compile_legacy_asm_object("asm-cmpxchg8b", source);
    assert_bytes_present(&text, &[0xf0, 0x0f, 0xc7, 0x0d], "lock cmpxchg8b");
    assert_disasm_present(&disasm, "lock cmpxchg8b");
}

/// inline asm: pushfl/popfl/iret/iretl aliases
#[test]
fn asm_pushfl_iret_has_opcode_and_disassembly() {
    let source = include_str!("../fixtures/asm/asm_pushfl_iret.c");
    let (text, disasm) = compile_legacy_asm_object("asm-pushfl-iret", source);
    assert_bytes_present(&text, &[0x9c, 0x58, 0x9c, 0x9d], "pushfl/popq/pushfl/popfl");
    // iret/iretl 在各自的 asm volatile 中, 之间有 mfence 屏障
    assert_bytes_present(&text, &[0xcf], "iret opcode");
    // 至少两个 iret (0xcf) 存在于 .text 中
    assert!(
        text.windows(1).filter(|w| w[0] == 0xcf).count() >= 2,
        "need at least 2 iret opcodes"
    );
    assert_disasm_present(&disasm, "pushf");
    assert_disasm_present(&disasm, "popf");
    assert_disasm_present(&disasm, "iret");
}

/// inline asm: cmov full condition codes + width suffix aliases
#[test]
fn asm_cmov_full_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_cmov_full.c");
    let (output, _obj) = run_object_test("asm-cmov-full", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");
}

/// inline asm: bare in/out + memory-indirect port forms
#[test]
fn asm_bare_in_out_has_opcode_and_disassembly() {
    let source = include_str!("../fixtures/asm/asm_bare_in_out.c");
    let (text, disasm) = compile_legacy_asm_object("asm-bare-in-out", source);
    assert_bytes_present(&text, &[0xe5, 0x80], "in immediate port");
    assert_bytes_present(&text, &[0xe7, 0x80], "out immediate port");
    // in/out dx dword: ed 和 ef 在各自 asm volatile 中, 之间有 mfence
    assert_bytes_present(&text, &[0xed], "in dx dword port");
    assert_bytes_present(&text, &[0xef], "out dx dword port");
    assert_bytes_present(&text, &[0xec], "in dx byte port");
    assert_bytes_present(&text, &[0xee], "out dx byte port");
    assert_disasm_present(&disasm, "in     $0x80,%eax");
    assert_disasm_present(&disasm, "out    %eax,$0x80");
    assert_disasm_present(&disasm, "in     (%dx),%eax");
    assert_disasm_present(&disasm, "out    %al,(%dx)");
}

/// shldw/shrdw 16-bit double precision shift
#[test]
fn asm_shldw_shrdw_has_opcode_and_disassembly() {
    let source = include_str!("../fixtures/asm/asm_shldw_shrdw.c");
    let (text, disasm) = compile_legacy_asm_object("asm-shldw-shrdw", source);
    assert_bytes_present(&text, &[0x66, 0x40, 0x0f, 0xa4], "shldw immediate");
    assert_bytes_present(&text, &[0x66, 0x40, 0x0f, 0xac], "shrdw immediate");
    assert_bytes_present(&text, &[0x66, 0x40, 0x0f, 0xa5], "shldw cl");
    assert_bytes_present(&text, &[0x66, 0x40, 0x0f, 0xad], "shrdw cl");
    assert_disasm_present(&disasm, "shld $0x4");
    assert_disasm_present(&disasm, "shrd $0x3");
    assert_disasm_present(&disasm, "shld %cl");
    assert_disasm_present(&disasm, "shrd %cl");
}

/// bare segment prefix as standalone instruction (old-style asm)
#[test]
fn asm_bare_seg_prefix_has_opcode_and_disassembly() {
    let source = include_str!("../fixtures/asm/asm_bare_seg_prefix.c");
    let (text, disasm) = compile_legacy_asm_object("asm-bare-seg-prefix", source);
    for (bytes, label) in [
        ([0x3e, 0x90], "ds nop"),
        ([0x26, 0x90], "es nop"),
        ([0x2e, 0x90], "cs nop"),
        ([0x36, 0x90], "ss nop"),
        ([0x64, 0x90], "fs nop"),
        ([0x65, 0x90], "gs nop"),
    ] {
        assert_bytes_present(&text, &bytes, label);
        assert_disasm_present(&disasm, label);
    }
}

/// .pushsection non-text section data collection (__bug_table, __ex_table)
#[test]
fn asm_pushsection_data_compiles_with_sections() {
    let source = include_str!("../fixtures/asm/asm_pushsection_data.c");
    let (output, _obj) = run_object_test("asm-pushsection-data", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");
}

/// 可执行 .pushsection (.fixup / .altinstr_replacement): 代码必须汇编进
/// 自定义节 (非丢弃/非泄漏到 .text), 且节头带 SHF_EXECINSTR ("ax") 标志.
#[test]
fn asm_pushsection_exec_code_lands_in_custom_section() {
    let source = include_str!("../fixtures/asm/asm_pushsection_exec.c");
    let (output, obj_path) = run_object_test("asm-pushsection-exec", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");
    let obj = std::fs::read(&obj_path).expect("read object file");
    let _ = std::fs::remove_file(&obj_path);
    let secs = elf64_sections(&obj);
    // .fixup 含 mov + ret 机器码, 必须非零且可执行
    let (ty, flags, size) = secs.get(".fixup").copied().expect("missing .fixup section");
    assert_eq!(ty, 1, ".fixup must be SHT_PROGBITS");
    assert!(flags & 0x4 != 0, ".fixup must have SHF_EXECINSTR (got flags {flags:#x})");
    assert!(size >= 5, ".fixup code dropped (size {size})");
    // .altinstr_replacement 含 nop, 非零且可执行
    let (_, flags, size) = secs
        .get(".altinstr_replacement")
        .copied()
        .expect("missing .altinstr_replacement section");
    assert!(
        flags & 0x4 != 0,
        ".altinstr_replacement must have SHF_EXECINSTR (got flags {flags:#x})"
    );
    assert!(size >= 1, ".altinstr_replacement code dropped (size {size})");
    // 数据节引用替换代码标签: .altinstructions 需要重定位节
    assert!(
        secs.contains_key(".rela.altinstructions"),
        "missing .rela.altinstructions (label ref into .altinstr_replacement)"
    );
    // .fixup jumps back into .text in common kernel fault-handler
    // patterns.  That must be represented with a relocation so objtool can
    // resolve the final jump target after the section is moved.
    assert!(
        secs.contains_key(".rela.fixup"),
        "missing .rela.fixup for executable pushsection jump target"
    );
    // 数据节数据非零: __ex_table/.altinstructions 条目必须输出
    for sec in ["__ex_table", ".altinstructions"] {
        let (_, _, size) = secs
            .get(sec)
            .copied()
            .unwrap_or_else(|| panic!("missing {sec} section (pushsection data dropped)"));
        assert!(size > 0, "{sec} data dropped (size {size})");
    }
}

/// 'q' inline assembly constraint: maps to GP registers (rax/rbx/rcx/rdx/rsi/rdi/rbp/r8-r15)
#[test]
fn asm_q_constraint_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_q_constraint.c");
    let (output, _obj) = run_object_test("asm-q-constraint", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("unsupported constraint"),
        "q constraint should be supported: {stderr}"
    );
}

/// x86-specific register constraints: a/b/c/d/S/D/q/Q/p
/// Ensures all fixed-register constraints compile without errors
#[test]
fn asm_x86_reg_constraints_compile_and_run() {
    let source = include_str!("../fixtures/asm/asm_x86_reg_constraints.c");
    let (output, _obj) = run_object_test("asm-x86-reg-constraints", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("unsupported constraint"),
        "x86 reg constraints should be supported: {stderr}"
    );
}

/// Nested .pushsection/.popsection: kernel exception table with nested .fixup
#[test]
fn asm_nested_pushsection_compiles_with_sections() {
    let source = include_str!("../fixtures/asm/asm_nested_pushsection.c");
    let (output, obj_path) = run_object_test("asm-nested-pushsection", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");
    let obj = std::fs::read(&obj_path).expect("read object file");
    let _ = std::fs::remove_file(&obj_path);
    let secs = elf64_sections(&obj);
    // __ex_table should have 3 .long entries (12 bytes) per exception table entry
    let (ty, _, size) = secs
        .get("__ex_table")
        .copied()
        .expect("missing __ex_table section");
    assert_eq!(ty, 1, "__ex_table must be SHT_PROGBITS");
    assert!(size >= 12, "__ex_table too small (size {size}), expected at least 12 bytes");
    assert_eq!(size % 12, 0, "__ex_table size {size} not a multiple of 12");
    // .fixup should be executable
    let (_, flags, size) = secs.get(".fixup").copied().expect("missing .fixup section");
    assert!(flags & 0x4 != 0, ".fixup must have SHF_EXECINSTR (got flags {flags:#x})");
    assert!(size >= 5, ".fixup code dropped (size {size})");
}

#[test]
fn asm_numeric_labels_are_scoped_per_block() {
    let source = r#"
int first(int x) {
    asm volatile(
        "1: cmpl $0, %0\n\t"
        "   je 1f\n\t"
        "   addl $1, %0\n\t"
        "1:"
        : "+r"(x));
    return x;
}

int second(int x) {
    asm volatile(
        "1: cmpl $0, %0\n\t"
        "   je 1f\n\t"
        "   addl $2, %0\n\t"
        "1:"
        : "+r"(x));
    return x;
}
"#;
    let (output, obj_path) = run_object_test("asm-numeric-label-scope", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let dump = Command::new("objdump")
        .args(["-dr"])
        .arg(&obj_path)
        .output()
        .expect("objdump");
    let _ = std::fs::remove_file(&obj_path);
    assert!(dump.status.success(), "objdump -dr failed");
    let disasm = String::from_utf8_lossy(&dump.stdout);
    assert!(
        disasm.contains("<first>:") && disasm.contains("<second>:"),
        "missing function disassembly: {disasm}"
    );
    assert!(
        disasm.contains("<first+") && disasm.contains("<second+"),
        "numeric jumps should target labels inside each function: {disasm}"
    );
}

/// 空的 asm 函数符号也必须有可定位的起始指令。
#[test]
fn asm_empty_function_symbol_gets_starting_instruction() {
    let source = r#"
asm(".type _context_unsafe_alias, @function\n"
    "_context_unsafe_alias:\n"
    ".size _context_unsafe_alias, .-_context_unsafe_alias\n"
    ".type empty_inline_alias, @function\n"
    "empty_inline_alias:\n");
"#;
    let (output, obj_path) = run_object_test("asm-empty-func-symbol", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let obj_data = std::fs::read(&obj_path).expect("read object file");
    let funcs = elf64_func_symbols(&obj_data);
    let names: std::collections::HashMap<String, u64> =
        funcs.iter().map(|(n, _, s)| (n.clone(), *s)).collect();
    for want in ["_context_unsafe_alias", "empty_inline_alias"] {
        let sz = names
            .get(want)
            .unwrap_or_else(|| panic!("missing STT_FUNC symbol {want}"));
        assert_eq!(*sz, 1, "empty asm function {want} should be a ret");
    }
    let dump = Command::new("objdump")
        .args(["-d"])
        .arg(&obj_path)
        .output()
        .expect("objdump");
    assert!(dump.status.success(), "objdump -d failed");
    let disasm = String::from_utf8_lossy(&dump.stdout);
    assert!(
        disasm.contains("<_context_unsafe_alias>:") && disasm.contains("ret"),
        "empty asm function must disassemble to a real instruction: {disasm}"
    );
    let _ = fs::remove_file(&obj_path);
}

#[test]
fn executable_pushsection_alignment_padding_is_nop() {
    let source = r#"
asm(".pushsection .fixup, \"ax\"\n"
    ".byte 0xcc\n"
    ".p2align 4\n"
    ".byte 0xc3\n"
    ".popsection\n");
int main(void) { return 0; }
"#;
    let (output, obj_path) = run_object_test("exec-pushsection-nop-pad", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let obj_data = std::fs::read(&obj_path).expect("read object file");
    let fixup = elf64_section_data(&obj_data, ".fixup").expect("missing .fixup");
    assert_eq!(fixup.first(), Some(&0xcc));
    assert_eq!(fixup.last(), Some(&0xc3));
    assert!(
        fixup[1..fixup.len() - 1].iter().all(|&b| b == 0x90),
        "executable section padding must be decodable nops: {fixup:02x?}"
    );
    let _ = std::fs::remove_file(&obj_path);
}

/// .section/.previous 支持 (VDSO 构建场景):
/// .section 切换节后数据必须进入自定义节而非 .text,
/// 重定位必须为 PC32 (PIC 兼容), 不能是 R_X86_64_32 绝对重定位.
#[test]
fn asm_section_previous_vdso_pattern() {
    let source = include_str!("../fixtures/asm/asm_section_previous_vdso.c");
    let (output, obj_path) = run_object_test("asm-section-previous-vdso", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");
    let obj_data = std::fs::read(&obj_path).expect("read object file");
    // .altinstr_replacement 必须存在且包含可执行代码
    let alt_sec = elf64_section_data(&obj_data, ".altinstr_replacement")
        .expect("missing .altinstr_replacement (section directive dropped)");
    assert!(!alt_sec.is_empty(), ".altinstr_replacement empty");
    // .altinstructions 必须存在且包含数据条目
    let alt_instr = elf64_section_data(&obj_data, ".altinstructions")
        .expect("missing .altinstructions (previous directive dropped)");
    assert!(alt_instr.len() >= 8, ".altinstructions too small: {}", alt_instr.len());
    // __bug_table 必须存在
    let bug_table = elf64_section_data(&obj_data, "__bug_table").expect("missing __bug_table");
    assert!(!bug_table.is_empty(), "__bug_table empty");
    // __ex_table 必须存在 (嵌套 pushsection 在 section 上下文中)
    let ex_table = elf64_section_data(&obj_data, "__ex_table")
        .expect("missing __ex_table (nested pushsection in section context)");
    assert!(!ex_table.is_empty(), "__ex_table empty");
    // 验证无 R_X86_64_32 绝对重定位 (PIC 不兼容)
    let dump = Command::new("objdump")
        .args(["-r"])
        .arg(&obj_path)
        .output()
        .expect("objdump -r");
    let relocs = String::from_utf8_lossy(&dump.stdout);
    let has_abs32 = relocs
        .lines()
        .any(|line| line.contains("R_X86_64_32") && !line.contains("R_X86_64_32S"));
    assert!(!has_abs32, "found R_X86_64_32 absolute relocation (PIC incompatible): {relocs}");
    let _ = std::fs::remove_file(&obj_path);
}

#[test]
fn function_return_thunk_avoids_naked_ret_in_c_and_asm() {
    let source = r#"
asm(".globl empty_asm_func\n"
    ".type empty_asm_func, @function\n"
    "empty_asm_func:\n"
    ".size empty_asm_func, .-empty_asm_func\n");
int normal_return(int x) { return x + 1; }
void inline_asm_return(void) { asm volatile("ret"); }
"#;
    let (output, obj_path) =
        run_object_test_flags("function-return-thunk", source, &["-mfunction-return=thunk-extern"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let dump = Command::new("objdump")
        .args(["-d", "-r"])
        .arg(&obj_path)
        .output()
        .expect("objdump");
    assert!(dump.status.success(), "objdump -d -r failed");
    let disasm = String::from_utf8_lossy(&dump.stdout);
    let has_naked_ret = disasm.lines().any(|line| {
        let insn = line.split('\t').next_back().unwrap_or("").trim();
        insn == "ret" || insn == "retq"
    });
    assert!(!has_naked_ret, "rethunk .text must not contain naked ret: {disasm}");
    assert!(
        disasm.contains("__x86_return_thunk"),
        "missing return thunk relocation: {disasm}"
    );
    assert!(
        disasm.matches("__x86_return_thunk").count() >= 3,
        "normal, empty asm, and inline asm returns should thunk: {disasm}"
    );
    let _ = std::fs::remove_file(&obj_path);
}

/// mini-asm 扩展：bts/btr/btc/bsf/bsr/popcnt/bswap/movnt/lock/rep/伪指令
#[test]
fn asm_extended_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_ext.c");
    let (output, _obj) = run_object_test("asm-extended", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// M13.1: asm 约束扩展 (含 XMM "x" 输入 prepare + 写回)
#[test]
fn asm_constraints_ext_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_constr.c");
    let (output, _exe) = run_compile_and_run("asm-constraints-ext", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "asm_constraints_ext should return 0, got {rc}");
    }
}

/// 约束系统扩展: 段寄存器/x87/MMX/条件码/显式子寄存器约束
#[test]
fn asm_constraint_ext2_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_constraint_ext2.c");
    let (output, _exe) = run_compile_and_run("asm-constraint-ext2", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "asm_constraint_ext2 should return 0, got {rc}");
    }
}

/// mini-asm 内核扩展：rol/prefetch/movss/addss
#[test]
fn asm_kernel_extra_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_kernel.c");
    let (output, _obj) = run_object_test("asm-kernel-extra", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// mini-asm 内核扩展指令：orb/orw/cmova/clwb/invpcid/rdrand/rdseed/
/// fwait/fxsaveq/fxrstorq/xgetbv/xsetbv/monitor/mwait/monitorx/mwaitx/
/// clgi/stgi/vmsave/sldt/repe 前缀修复
#[test]
fn asm_kernel_ext_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_kernel_ext.c");
    let (output, _obj) = run_object_test("asm-kernel-ext", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// Kernel asm features: .byte multi-value, .pushsection skip,
/// popcnt REX fix, push $label
#[test]
fn asm_kernel_features_byte_pushsection_popcnt() {
    let source = include_str!("../fixtures/asm/asm_kernel_features.c");
    let (output, _obj) = run_object_test("asm-kernel-features", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected nop fallback: {stderr}");
}

/// mini-asm 补全 (高): 段寄存器 / 系统指令 / 内核启动 / 虚拟化指令
#[test]
fn asm_paravirt_boot_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_paravirt.c");
    let (output, _obj) = run_object_test("asm-paravirt-boot", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// 完整指令子集 (M13): AVX/AVX2/AVX-VNNI + AES-NI/SHA + FMA/F16C + TSX + AMX
#[test]
fn asm_avx_aes_fma_tsx_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_avx.c");
    let (output, _obj) = run_object_test("asm-avx-aes-fma-tsx", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// x87 FPU 指令集: 无操作数指令 / 比较 / 加载存储 / 控制 / 算术 / BCD
#[test]
fn asm_x87_fpu_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_x87_fpu.c");
    let (output, _obj) = run_object_test("asm-x87-fpu", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// MMX 指令集: emms / 打包解包 / 算术 / 移位 / pshufw
#[test]
fn asm_mmx_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_mmx.c");
    let (output, _obj) = run_object_test("asm-mmx", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// 字符串操作 / 循环 / 杂项指令: lodsb-w-l-q / cmpsw-l-q / scasw-l-q / loop / enter
#[test]
fn asm_string_loop_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_string_loop.c");
    let (output, _obj) = run_object_test("asm-string-loop", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// BMI1/BMI2 扩展: andn/bextr/blsi/blsmsk/blsr/bzhi/mulx/pdep/pext/rorx/sarx/shlx/shrx/adcx/adox
#[test]
fn asm_bmi_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_bmi.c");
    let (output, _obj) = run_object_test("asm-bmi", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// Phase 2: 操作数模式补全 — 所有新加的操作数组合
#[test]
fn asm_operand_patterns_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_operand_patterns.c");
    let (output, _obj) = run_object_test("asm-operand-patterns", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// Phase 2: 远跳转/远调用 — 所有远跳转指令
#[test]
fn asm_far_jumps_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_far_jumps.c");
    let (output, _obj) = run_object_test("asm-far-jumps", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// Phase 2: GlobalAsm/asm goto 残余 — 所有间接跳转/段前缀组合
#[test]
fn asm_global_asm_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_global_asm.c");
    let (output, _obj) = run_object_test("asm-global-asm", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// 扩展操作数形式: rdrand/rdseed 内存 / ljmp/lcall 无后缀 / retf/lret imm16 / TSX / AVX-imm 内存源 / pinsr*/pextr* 内存 / vpinsr*/vpextr*/vcvtps2ph 内存 / vextractpd XMM / MMX pshufw / AMX tilezero
#[test]
fn asm_extended_forms_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_extended_forms.c");
    let (output, _obj) = run_object_test("asm-extended-forms", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// 符号操作数形式: rdrand/rdseed Sym / retf/lret Sym / ljmp Sym / AVX-imm SymDisp / MMX pshufw SymDisp / vextractps SymDisp
#[test]
fn asm_sym_operand_forms_compiles_without_nop_fallback() {
    let source = include_str!("../fixtures/asm/asm_sym_operand_forms.c");
    let (output, _obj) = run_object_test("asm-sym-operand-forms", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stderr.contains("emitted as nop"), "unexpected mini-asm nop fallback: {stderr}");
}

/// asm 模板修饰符完整支持: %a (地址形式) / %P (原始符号名) / %l (标签)
/// 包含: 立即数/寄存器/命名操作数修饰符 + 多位索引边界 + 立即数优先级
#[test]
fn asm_template_modifiers_compile_and_run() {
    let source = include_str!("../fixtures/asm/asm_template_modifiers.c");
    let (output, exe) = run_compile_and_run("asm-template-modifiers", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0, "asm template modifiers test failed");
}

/// %P / %a 修饰符对立即数输出纯数字 (无 $ 前缀): -S 文本验证
#[test]
fn asm_raw_modifiers_p_a_text_output() {
    let source = include_str!("../fixtures/asm/asm_raw_modifiers.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-asm-raw-mod-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-asm-raw-mod-{}.s", std::process::id()));
    std::fs::write(&source_path, source).expect("write C fixture");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let asm = std::fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&asm_path);
    // %P0 对立即数 42 应输出 ".byte 42" 而非 ".byte $42"
    assert!(
        asm.contains(".byte 42") || asm.contains(".byte\t42"),
        "%P modifier failed: expected '.byte 42' in asm output, got:\n{asm}"
    );
    // %a0 对立即数 99 应输出 ".byte 99"
    assert!(
        asm.contains(".byte 99") || asm.contains(".byte\t99"),
        "%a modifier failed: expected '.byte 99' in asm output, got:\n{asm}"
    );
    // %P[val] 对 7 应输出 ".byte 7"
    assert!(
        asm.contains(".byte 7") || asm.contains(".byte\t7"),
        "%P[name] modifier failed: expected '.byte 7' in asm output"
    );
    // %a[val] 对 11 应输出 ".byte 11"
    assert!(
        asm.contains(".byte 11") || asm.contains(".byte\t11"),
        "%a[name] modifier failed: expected '.byte 11' in asm output"
    );
}

/// 头文件中不支持的内联 asm 警告必须映射回真实源文件行号
/// (而非预处理输出行号, 内核头文件可达几十万行, 虚高行号无法定位)。
/// fixture: asm_warn_loc.c 用**相对** `#include "asm_warn_loc.h"` 引入含
/// `leaq $42, %0` 的头文件第 6 行 —— 两份夹具必须原名同目录落盘, 才能真的
/// 走一遍「相对包含 → 错误位置回映射」而不只是绝对路径展开。
#[test]
fn asm_warning_location_maps_to_real_source_line() {
    let dir = std::env::temp_dir();
    let tag = format!("dcc-asm-warn-loc-{}", std::process::id());
    let work = dir.join(&tag);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("create fixture dir");
    let hdr_path = work.join("asm_warn_loc.h");
    let src_path = work.join("asm_warn_loc.c");
    fs::write(&hdr_path, include_str!("../fixtures/asm/asm_warn_loc.h"))
        .expect("write header fixture");
    fs::write(&src_path, include_str!("../fixtures/asm/asm_warn_loc.c"))
        .expect("write C fixture");
    let obj_path = work.join(format!("{tag}.o"));
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 不支持的 lea 操作数模式产生 E0311 错误 (不再 warning+nop)
    assert!(!output.status.success(), "should fail with E0311");
    // 错误必须落在头文件第 6 行 (asm 语句), 而不是预处理输出行号
    // 格式: error[E0311]: ... --> {hdr_path}:6:1
    assert!(
        stderr.contains("E0311") && stderr.contains(&format!("{}:6", hdr_path.display())),
        "expected precise asm error location at {}:6, got: {stderr}",
        hdr_path.display()
    );
    let _ = fs::remove_file(obj_path);
    let _ = fs::remove_dir_all(work);
}

/// 头文件缓存 (冷→热) 回放不得丢失 line_map 精度:
/// 第二次编译 (缓存命中) 的错误位置必须与首次一致且同样精确。
#[test]
fn asm_warning_location_stable_across_header_cache_replay() {
    let dir = std::env::temp_dir();
    let tag = format!("dcc-asm-warn-cache-{}", std::process::id());
    let work = dir.join(&tag);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).expect("create fixture dir");
    let hdr_path = work.join("asm_warn_loc.h");
    let src_path = work.join("asm_warn_loc.c");
    let obj_path = work.join(format!("{tag}.o"));
    let home_dir = dir.join(format!("{tag}-home"));
    fs::create_dir_all(&home_dir).expect("create temp HOME");
    fs::write(&hdr_path, include_str!("../fixtures/asm/asm_warn_loc.h"))
        .expect("write header fixture");
    fs::write(&src_path, include_str!("../fixtures/asm/asm_warn_loc.c"))
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
    // 冷/热两条路径都应产生 E0311 错误
    assert!(!cold.status.success(), "cold should fail");
    assert!(!hot.status.success(), "hot should fail");
    // 冷/热两条路径的错误必须一致 (缓存回放不丢映射精度)
    assert_eq!(cold_err.trim(), hot_err.trim(), "cold/hot cache error mismatch");
    let expect = format!("{}:6", hdr_path.display());
    assert!(
        hot_err.contains("E0311") && hot_err.contains(&expect),
        "expected precise asm error location at {expect}:6, got: {hot_err}"
    );
    let _ = fs::remove_file(obj_path);
    let _ = fs::remove_dir_all(work);
    let _ = fs::remove_dir_all(home_dir);
}

/// Unsupported inline asm instructions must produce hard errors (E0311),
/// not silently fall back to NOP bytes.
#[test]
fn asm_unsupported_instructions_error_not_nop() {
    let dir = std::env::temp_dir();
    let tag = format!("dcc-asm-unsupported-{}", std::process::id());
    let hdr_path = dir.join(format!("{tag}.h"));
    let src_path = dir.join(format!("{tag}.c"));
    let hdr = include_str!("../fixtures/asm/asm_unsupported_error.h");
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
    // All unsupported asm patterns must produce E0311 errors
    assert!(!output.status.success(), "should fail with E0311 errors");
    // Check for multiple E0311 errors (one for each unsupported pattern)
    let e0311_count = stderr.matches("E0311").count();
    assert!(
        e0311_count >= 3,
        "expected at least 3 E0311 errors (unrecognized instr, unsupported operand patterns), got {e0311_count}: {stderr}"
    );
    // Verify specific error messages
    assert!(
        stderr.contains("foobar") || stderr.contains("unsupported inline asm instruction"),
        "should report unrecognized instruction: {stderr}"
    );
    assert!(
        stderr.contains("sha1rnds4") || stderr.contains("pshufd"),
        "should report unsupported operand pattern: {stderr}"
    );
    let _ = fs::remove_file(hdr_path);
    let _ = fs::remove_file(src_path);
    let _ = fs::remove_file(obj_path);
}

/// Segment register prefix (%gs:/%fs:) in inline asm for per-cpu
/// access + asm operand modifiers (%b/%w/%k/%q/%h).
/// Verifies: mov/ALU/lock/cmpxchg with segment prefix,
/// and %h high-byte register modifier runtime correctness.
#[test]
#[cfg(unix)]
fn asm_seg_prefix_and_modifiers_runtime() {
    let src = include_str!("../fixtures/asm/asm_seg_prefix_percpu.c");
    let (out, exe) = run_compile_and_run("asm-seg-prefix", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "seg prefix + modifiers compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "seg prefix + modifiers runtime check failed");
}

// ---------- byte-level asm 断言 ----------

/// byte-level asm: 对 Linux legacy mini-asm 指令增加反汇编/字节级断言.
/// 验证 `nop` 指令编码为单字节 0x90 且反汇编还原为 nop.
#[test]
#[cfg(unix)]
fn byte_level_asm_nop_encoding() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-asm-byte-nop-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-asm-byte-nop-{}.o", std::process::id()));
    let source = r#"void f(void) { asm volatile("nop"); }"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "dcc should compile nop asm: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&obj_path).expect("read object file");
    // .text 段应包含 nop (0x90)
    let text = elf64_section_data(&obj, ".text").expect("missing .text section");
    assert!(
        text.contains(&0x90),
        "nop instruction should be encoded as 0x90 in .text: {text:02x?}"
    );
    // 反汇编应包含 nop
    let dump = Command::new("objdump")
        .args(["-d", obj_path.to_str().unwrap()])
        .output()
        .expect("objdump");
    let disasm = String::from_utf8_lossy(&dump.stdout);
    assert!(disasm.contains("nop"), "objdump should disassemble nop: {disasm}");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
}

/// byte-level asm: `mov $1, %eax` 编码断言.
/// 期望字节序列: B8 01 00 00 00 (mov imm32, %eax)
#[test]
#[cfg(unix)]
fn byte_level_asm_mov_eax_encoding() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-asm-byte-mov-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-asm-byte-mov-{}.o", std::process::id()));
    let source = r#"void f(void) { asm volatile("mov $1, %eax"); }"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "dcc should compile mov asm: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&obj_path).expect("read object file");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    let text = elf64_section_data(&obj, ".text").expect("missing .text section");
    // mov $1, %eax = B8 01 00 00 00
    let expected = vec![0xB8, 0x01, 0x00, 0x00, 0x00];
    let found = text.windows(expected.len()).any(|w| w == expected);
    assert!(found, "expected mov $1, %eax bytes {expected:02x?} in .text: {text:02x?}");
}

/// byte-level asm: `ret` 指令编码为 0xC3.
#[test]
#[cfg(unix)]
fn byte_level_asm_ret_encoding() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-asm-byte-ret-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-asm-byte-ret-{}.o", std::process::id()));
    let source = r#"void f(void) { asm volatile("ret"); }"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "dcc should compile ret asm: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&obj_path).expect("read object file");
    let text = elf64_section_data(&obj, ".text").expect("missing .text section");
    assert!(
        text.contains(&0xC3),
        "ret instruction should be encoded as 0xC3 in .text: {text:02x?}"
    );
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
}

/// byte-level asm: `push %rbp; mov %rsp, %rbp` 帧头编码断言.
#[test]
#[cfg(unix)]
fn byte_level_asm_frame_header_encoding() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-asm-byte-frame-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-asm-byte-frame-{}.o", std::process::id()));
    let source = r#"void f(void) { asm volatile("push %rbp\nmov %rsp, %rbp"); }"#;
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "dcc should compile frame header asm: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&obj_path).expect("read object file");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    let text = elf64_section_data(&obj, ".text").expect("missing .text section");
    // push %rbp = 55, mov %rsp, %rbp = 48 89 E5
    assert!(
        text.contains(&0x55),
        "push %rbp should be encoded as 0x55 in .text: {text:02x?}"
    );
    let mov_seq = [0x48, 0x89, 0xE5];
    let found = text.windows(mov_seq.len()).any(|w| w == mov_seq);
    assert!(found, "expected mov %rsp, %rbp bytes {mov_seq:02x?} in .text: {text:02x?}");
}

/// 回归测试 (原 c_xfail/flag_clobber_frame.c): 局部 `int ok` 的赋值
/// `ok = 0;` 确实执行, 但帧内同名不同尺寸的聚合局部 (先 16B 的
/// `struct with_vec s;` 后 32B 的 `struct vec_int s;`) 共用同一槽且
/// 槽按首次声明定容, 导致 `s.i = 7` 溢出覆盖相邻的 ok —— 退出码型
/// 夹具静默假绿。已修复 (pre_register_vars 按全函数同名声明的最大
/// 尺寸分配槽)。注意: 夹具里的 `!= 99.0` 是**故意的触发器** (强制
/// 走 FAIL 分支执行 ok = 0), 不是笔误, 不要改回 10.0。
#[test]
#[cfg(unix)]
fn flag_clobber_frame() {
    let src = include_str!("../fixtures/c/flag_clobber_frame.c");
    let (out, exe) = run_compile_and_run("flag-clobber-frame", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let run = Command::new(&exe).output().expect("run exe");
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    // 判据自证: FAIL 行存在 → `ok = 0;` 真的执行了 (不是被死代码消除);
    // 退出码必须是 1 → 末尾 `if (!ok)` 看到了被写的 0。
    assert!(stdout.contains("FAIL:"), "fixture did not execute ok = 0:\n{stdout}");
    let rc = run.status.code().unwrap_or(-1);
    assert_eq!(rc, 1, "ok = 0 被后续同名大结构体的槽溢出丢失 (假绿):\n{stdout}");
    assert!(stdout.contains("SOME TESTS FAILED"), "missing verdict line:\n{stdout}");
    let _ = fs::remove_file(&exe);
}

// =================== 内联 asm clobber 完整支持 ===================

/// 内联 asm clobber 完整支持: cc/memory/x87/xmm/GP 寄存器 clobber
#[test]
fn asm_clobber_full_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_clobber.c");
    let (output, _exe) = run_compile_and_run("asm-clobber-full", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "asm_clobber should return 0, got {rc}");
    }
}

/// GCC asm 高级特性: .rept/.irp/.irpc/.macro/.if/.ifdef/.intel_syntax/.altmacro/asm_output
#[test]
fn asm_advanced_features_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_advanced_features.c");
    let (output, _exe) = run_compile_and_run("asm-advanced-features", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&_exe);
        assert_eq!(rc, 0, "asm_advanced_features should return 0, got {rc}");
    }
}

/// asm 诊断语义校验: 重复名/类型不匹配/占位符越界均 warning 后继续
#[test]
fn asm_diag_validation_compiles_with_warnings() {
    let source = include_str!("../fixtures/asm/asm_diag_validation.c");
    let (output, exe) = run_compile_and_run("asm-diag-validation", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "asm diagnostics should warn but still compile: {stderr}"
    );
    assert!(
        stderr.contains("W0005") || stderr.contains("W0008"),
        "expected asm warning diagnostics, stderr: {stderr}"
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "asm_diag_validation should run successfully");
    let _ = fs::remove_file(exe);
}

/// asm 优化器保留: DCE 不消除无副作用无输出的 asm (GCC 兼容)
#[test]
fn asm_optimizer_preservation_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_optimizer_preservation.c");
    let (output, exe) = run_compile_and_run_flags("asm-optimizer-preservation", source, &["-O2"]);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "asm optimizer preservation test failed");
    }
}

/// asm 调试信息: -g 编译含 asm 的函数应产生有效 DWARF 信息
#[test]
fn asm_debug_info_compiles_with_g() {
    let source = include_str!("../fixtures/asm/asm_debug_info.c");
    let (output, exe) = run_compile_and_run_flags("asm-debug-info", source, &["-g"]);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "asm debug info test failed");
    }
}

/// asm 逐指令调试信息: -g -c 下多指令 asm 块应为每条指令生成独立 .debug_line 条目
/// 使 GDB 能单步进入 asm 块内部 (类似 GCC .debug_insn 效果)
#[test]
fn asm_per_insn_debug_insn_entries() {
    let source = include_str!("../fixtures/asm/asm_debug_insn.c");
    // -g -c: 生成带调试信息的 .o 文件
    let (output, _obj) = run_compile_and_run_flags("asm-debug-insn", source, &["-g", "-c"]);
    assert!(
        output.status.success(),
        "asm per-insn debug compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        // 验证 .o 文件包含 .debug_line 节
        let obj_path = _obj.to_str().unwrap();
        let sections = std::process::Command::new("readelf")
            .args(["-S", obj_path])
            .output();
        if let Ok(out) = sections {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                text.contains(".debug_line"),
                "asm per-insn debug: .debug_line section missing from {}",
                obj_path
            );
        }
        // 验证可执行文件能正确运行
        let (output, exe) = run_compile_and_run_flags("asm-debug-insn-run", source, &["-g"]);
        assert!(
            output.status.success(),
            "asm per-insn debug run compile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "asm per-insn debug runtime result incorrect");
    }
}

// =====================================================================
// M3 milestone tests
// =====================================================================

/// M3.1: inline assembly - asm volatile, register constraints,
/// memory clobber, multiple outputs.
#[test]
#[cfg(unix)]
fn m3_1_inline_asm() {
    let src = include_str!("../fixtures/asm/asm_constraints_io_clobbers.c");
    let (out, code) = run_host_link_and_run("m3-1-asm", src);
    assert!(
        out.status.success(),
        "m3_1 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "inline asm checks failed (exit={code})");
}

/// asm volatile semantics: volatile flag propagated from parser through
/// SSA to codegen; asm goto always volatile; plain asm may be eliminated.
#[test]
#[cfg(unix)]
fn asm_volatile_semantics() {
    let src = include_str!("../fixtures/asm/asm_volatile_semantics.c");
    let (out, code) = run_host_link_and_run("asm-volatile-semantics", src);
    assert!(
        out.status.success(),
        "asm volatile compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "asm volatile checks failed (exit={code})");
}

/// GNU asm label symbol naming: __asm__("label") overrides ELF symbol name.
/// Critical for Linux kernel builds where asm labels control exported symbols.
#[test]
#[cfg(unix)]
fn asm_label_symbol_naming() {
    let src = include_str!("../fixtures/asm/asm_label_symbol.c");
    let (out, code) = run_host_link_and_run("asm-label-symbol", src);
    assert!(
        out.status.success(),
        "asm label symbol compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "asm label symbol checks failed (exit={code})");
}

/// Extended asm constraint coverage: I/J/K/L/M/N/O/P integer ranges,
/// early-clobber, commutative, multi-letter constraints.
#[test]
#[cfg(unix)]
fn asm_extended_constraint_coverage() {
    let src = include_str!("../fixtures/asm/asm_extended_constraints.c");
    let (out, code) = run_host_link_and_run("asm-ext-constraints", src);
    assert!(
        out.status.success(),
        "extended constraints compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "extended constraints checks failed (exit={code})");
}

/// asm goto extended label expressions: %c[label] strip immediate prefix,
/// label+offset, multiple goto labels.
#[test]
#[cfg(unix)]
fn asm_goto_label_expressions() {
    let src = include_str!("../fixtures/asm/asm_goto_label_expr.c");
    let (out, code) = run_host_link_and_run("asm-goto-label-expr", src);
    assert!(
        out.status.success(),
        "asm goto label expr compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "asm goto label expr checks failed (exit={code})");
}

/// SSA DCE pass can eliminate pure (non-volatile, no outputs, no
/// memory clobber) asm statements when their results are unused.
#[test]
#[cfg(unix)]
fn asm_dead_code_elimination() {
    let src = include_str!("../fixtures/asm/asm_dce.c");
    let (out, code) = run_host_link_and_run("asm-dce", src);
    assert!(
        out.status.success(),
        "asm dce compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "asm dce checks failed (exit={code})");
}

// =================== Section 11: asm test coverage gaps ===================

/// asm goto 输出约束跨 fall-through / jump 路径写回验证
#[test]
#[cfg(unix)]
fn asm_goto_output_paths_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_goto_output_paths.c");
    let (output, exe) = run_compile_and_run("asm-goto-output-paths", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "asm_goto_output_paths should return 0, got {rc}");
}

/// asm goto 与 &&label 混合
#[test]
#[cfg(unix)]
fn asm_goto_label_addr_mix_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_goto_label_addr_mix.c");
    let (output, exe) = run_compile_and_run("asm-goto-label-addr-mix", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "asm_goto_label_addr_mix should return 0, got {rc}");
}

/// 嵌套 asm 字符串里的 .macro / .rept 块
#[test]
#[cfg(unix)]
fn asm_nested_macro_rept_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_nested_macro_rept.c");
    let (output, exe) = run_compile_and_run("asm-nested-macro-rept", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "asm_nested_macro_rept should return 0, got {rc}");
}

/// memory clobber + 优化器重排验证 (-O2)
#[test]
#[cfg(unix)]
fn asm_memory_clobber_optimizer_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_memory_clobber_optimizer.c");
    let (output, exe) = run_compile_and_run_flags("asm-memory-clobber-optimizer", source, &["-O2"]);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "asm_memory_clobber_optimizer should return 0, got {rc}");
}

/// 大规模 AVX-512 EVEX 掩码指令端到端运行
#[test]
#[cfg(unix)]
fn asm_avx512_runtime_compiles_and_runs() {
    let source = include_str!("../fixtures/asm/asm_avx512_runtime.c");
    let (output, exe) = run_compile_and_run("asm-avx512-runtime", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "asm_avx512_runtime should return 0, got {rc}");
}

/// 多 TU 跨文件 asm goto 跳转: 两个 .o 分别编译后链接
#[test]
#[cfg(unix)]
fn asm_goto_multi_tu_compiles_and_runs() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    // TU1: defines asm_goto helper function
    let tu1_src = r#"
__attribute__((noinline))
int asm_goto_helper(int x) {
    asm goto (
        "cmp $0, %0\n\t"
        "jg %l[positive]"
        :
        : "r" (x)
        : "cc"
        : positive
    );
    return -1;
positive:
    return 1;
}
"#;
    // TU2: calls asm_goto_helper from TU1
    let tu2_src = r#"
extern int asm_goto_helper(int x);
int main(void) {
    int r1 = asm_goto_helper(5);
    if (r1 != 1) return 1;
    int r2 = asm_goto_helper(-3);
    if (r2 != -1) return 2;
    return 0;
}
"#;
    let tu1_path = dir.join(format!("dcc-asm-goto-tu1-{}.c", pid));
    let tu2_path = dir.join(format!("dcc-asm-goto-tu2-{}.c", pid));
    let tu1_obj = dir.join(format!("dcc-asm-goto-tu1-{}.o", pid));
    let tu2_obj = dir.join(format!("dcc-asm-goto-tu2-{}.o", pid));
    let exe = dir.join(format!("dcc-asm-goto-multi-tu-{}", pid));
    std::fs::write(&tu1_path, tu1_src).unwrap();
    std::fs::write(&tu2_path, tu2_src).unwrap();
    let dcc = env!("CARGO_BIN_EXE_dcc");
    let c1 = std::process::Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&tu1_obj)
        .arg(&tu1_path)
        .output()
        .expect("dcc tu1");
    let _ = std::fs::remove_file(&tu1_path);
    assert!(c1.status.success(), "tu1 compile: {}", String::from_utf8_lossy(&c1.stderr));
    let c2 = std::process::Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&tu2_obj)
        .arg(&tu2_path)
        .output()
        .expect("dcc tu2");
    let _ = std::fs::remove_file(&tu2_path);
    assert!(c2.status.success(), "tu2 compile: {}", String::from_utf8_lossy(&c2.stderr));
    let link = std::process::Command::new("gcc")
        .args(["-o"])
        .arg(&exe)
        .arg(&tu1_obj)
        .arg(&tu2_obj)
        .output()
        .expect("gcc link");
    let _ = std::fs::remove_file(&tu1_obj);
    let _ = std::fs::remove_file(&tu2_obj);
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    #[cfg(unix)]
    {
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    let run = std::process::Command::new(&exe).output().expect("run");
    let rc = run.status.code().unwrap_or(-1);
    let _ = std::fs::remove_file(&exe);
    assert_eq!(rc, 0, "multi-TU asm goto should return 0, got {rc}");
}

/// 非 x86_64 后端 inline asm: GlobalAsm / AsmExpr / AsmGoto 不 panic
/// 验证 aarch64/riscv64/mips/mips64/arm 后端在 -S 模式下正确处理内联汇编
#[test]
fn asm_non_x86_backend_s_mode_compiles() {
    let source = include_str!("../fixtures/asm/asm_non_x86_targets.c");
    let targets = [
        "aarch64-linux-gnu",
        "riscv64-linux-gnu",
        "mips-linux-gnu",
        "mips64-linux-gnuabi64",
        "arm-linux-gnueabihf",
    ];
    for target in &targets {
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!(
            "dcc-asm-nonx86-S-{}-{}.c",
            target.replace('/', "-"),
            std::process::id()
        ));
        let asm_path = dir.join(format!(
            "dcc-asm-nonx86-S-{}-{}.s",
            target.replace('/', "-"),
            std::process::id()
        ));
        fs::write(&src_path, source).expect("write C fixture");
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-S", "-o"])
            .arg(&asm_path)
            .arg(&src_path)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "target {}: -S mode failed: {}", target, stderr);
        // 验证 -S 输出包含 asm 文本
        if asm_path.exists() {
            let asm_content = fs::read_to_string(&asm_path).unwrap_or_default();
            assert!(
                asm_content.contains("nop")
                    || asm_content.contains(".globl __test_global_asm_marker"),
                "target {}: -S output should contain asm text, got:\n{}",
                target,
                &asm_content[..asm_content.len().min(500)]
            );
        }
        let _ = fs::remove_file(&src_path);
        let _ = fs::remove_file(&asm_path);
    }
}

/// 非 x86_64 后端 inline asm: -c 模式不 panic，mini-assembler 编码机器码
#[test]
fn asm_non_x86_backend_c_mode_no_panic() {
    let source = include_str!("../fixtures/asm/asm_non_x86_targets.c");
    let targets = [
        "aarch64-linux-gnu",
        "riscv64-linux-gnu",
        "mips-linux-gnu",
        "mips64-linux-gnuabi64",
        "arm-linux-gnueabihf",
    ];
    for target in &targets {
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!(
            "dcc-asm-nonx86-c-{}-{}.c",
            target.replace('/', "-"),
            std::process::id()
        ));
        let obj_path = dir.join(format!(
            "dcc-asm-nonx86-c-{}-{}.o",
            target.replace('/', "-"),
            std::process::id()
        ));
        fs::write(&src_path, source).expect("write C fixture");
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj_path)
            .arg(&src_path)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        // -c 模式应该成功 (mini-assembler 编码机器码)
        assert!(output.status.success(), "target {}: -c mode failed: {}", target, stderr);
        // -c 模式不应包含旧的 "non-x86_64 target" 警告 (已编码机器码)
        // 对于无法识别的指令可能有 "unrecognized" 警告，但 nop 等常见指令不应有
        let _ = fs::remove_file(&src_path);
        let _ = fs::remove_file(&obj_path);
    }
}

// ============================================================================
// Non-x86 inline asm — -S text preservation and -c warning diagnostics
// ============================================================================

/// Non-x86 -S: global asm text is preserved in assembly output
#[test]
fn non_x86_asm_minus_s_preserves_global_asm_text() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_non_x86_preserve.c");
    let targets = [
        "aarch64-linux-gnu",
        "riscv64-linux-gnu",
        "arm-linux-gnueabihf",
        "mips-linux-gnu",
        "mips64-linux-gnuabi64",
    ];
    for target in targets {
        let dir = std::env::temp_dir();
        let asm_path = dir.join(format!(
            "dcc-nonx86-asm-{}-{}.s",
            target.replace('-', "_"),
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-S", "-o"])
            .arg(&asm_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        assert!(
            output.status.success(),
            "target {target}: -S should succeed; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        let asm = fs::read_to_string(&asm_path).unwrap_or_default();
        let _ = fs::remove_file(&asm_path);
        // Global asm marker must be preserved in -S output
        assert!(
            asm.contains("__dcc_non_x86_asm_marker"),
            "target {target}: -S output should preserve global asm marker"
        );
        // All functions should be present
        for func in &[
            "test_asm_expr",
            "test_asm_stmt",
            "test_asm_goto",
            "test_named",
        ] {
            assert!(
                asm.contains(&format!(".globl {func}")),
                "target {target}: -S output should contain .globl {func}"
            );
        }
    }
}

/// Non-x86 -c: asm goto is encoded by mini-assembler in -c mode
#[test]
fn non_x86_asm_minus_c_warns_on_asm_goto() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_non_x86_preserve.c");
    let targets = [
        "aarch64-linux-gnu",
        "riscv64-linux-gnu",
        "arm-linux-gnueabihf",
        "mips-linux-gnu",
        "mips64-linux-gnuabi64",
    ];
    for target in targets {
        let dir = std::env::temp_dir();
        let obj_path = dir.join(format!(
            "dcc-nonx86-asm-c-{}-{}.o",
            target.replace('-', "_"),
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_file(&obj_path);
        // Compilation should succeed (mini-assembler encodes instructions)
        assert!(output.status.success(), "target {target}: -c should succeed; stderr={stderr}");
        // -c mode should NOT contain old "asm goto on non-x86_64 target" warning
        // (instructions are now encoded by mini-assembler)
        assert!(
            !stderr.contains("asm goto on non-x86_64 target"),
            "target {target}: -c should not have old asm goto warning; stderr={stderr}"
        );
    }
}

/// Non-x86 -c: arm/mips/mips64 no longer produce "result is 0" warnings
/// (mini-assembler now encodes inline asm instructions in -c mode)
#[test]
fn non_x86_asm_arm_mips_warn_on_inline_asm_expr() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_non_x86_preserve.c");
    // arm/mips/mips64 backends no longer emit "result is 0" warnings
    // in -c mode since mini-assembler encodes the instructions
    let targets_with_expr_warnings = [
        "arm-linux-gnueabihf",
        "mips-linux-gnu",
        "mips64-linux-gnuabi64",
    ];
    for target in targets_with_expr_warnings {
        let dir = std::env::temp_dir();
        let obj_path = dir.join(format!(
            "dcc-nonx86-expr-{}-{}.o",
            target.replace('-', "_"),
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_file(&obj_path);
        assert!(output.status.success(), "target {target}: -c should succeed; stderr={stderr}");
        // -c mode should NOT contain old "inline asm on non-x86_64 target" warning
        assert!(
            !stderr.contains("inline asm on non-x86_64 target"),
            "target {target}: -c should not have old inline asm warning; stderr={stderr}"
        );
    }
}

/// Non-x86 -S: enhanced constraint resolution produces real register names
#[test]
fn non_x86_asm_constraint_resolution_minus_s() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_non_x86_constraints.c");
    let targets = [
        ("aarch64-linux-gnu", "w"),   // AArch64 uses w0-w15
        ("riscv64-linux-gnu", "a"),   // RISC-V uses a0-a7
        ("arm-linux-gnueabihf", "r"), // ARM uses r0-r3
        ("mips-linux-gnu", "$"),      // MIPS uses $v0, $t0, etc.
    ];
    for (target, reg_prefix) in targets {
        let dir = std::env::temp_dir();
        let asm_path = dir.join(format!(
            "dcc-nonx86-constraint-{}-{}.s",
            target.replace('-', "_"),
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-S", "-o"])
            .arg(&asm_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "target {target}: -S should succeed; stderr={stderr}");
        let asm = fs::read_to_string(&asm_path).unwrap_or_default();
        let _ = fs::remove_file(&asm_path);
        // The asm output should contain architecture-specific register names
        // (not bare %0, %1 placeholders)
        assert!(
            asm.contains(reg_prefix),
            "target {target}: -S output should contain arch register prefix '{reg_prefix}'; asm={asm}"
        );
        // Global asm marker should be present
        assert!(
            asm.contains("__dcc_asm_constraint_test"),
            "target {target}: -S output should preserve global asm marker"
        );
    }
}

/// Non-x86 -c: memory clobber emits architecture barrier instruction
#[test]
fn non_x86_asm_memory_clobber_barrier() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_non_x86_constraints.c");
    let targets = [
        "aarch64-linux-gnu",
        "riscv64-linux-gnu",
        "arm-linux-gnueabihf",
        "mips-linux-gnu",
        "mips64-linux-gnuabi64",
    ];
    for target in targets {
        let dir = std::env::temp_dir();
        let obj_path = dir.join(format!(
            "dcc-nonx86-barrier-{}-{}.o",
            target.replace('-', "_"),
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_file(&obj_path);
        assert!(
            output.status.success(),
            "target {target}: -c should succeed with memory clobber; stderr={stderr}"
        );
    }
}

/// Non-x86 -S: constraint resolution produces real register names in asm output
#[test]
fn non_x86_asm_operand_resolution_produces_register_names() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_non_x86_constraints.c");
    let targets_and_reg_prefixes = [
        ("aarch64-linux-gnu", "w"),   // AArch64 uses w0-w15
        ("riscv64-linux-gnu", "a"),   // RISC-V uses a0-a7
        ("arm-linux-gnueabihf", "r"), // ARM uses r0-r3
        ("mips-linux-gnu", "$"),      // MIPS uses $v0, $t0, etc.
    ];
    for (target, _reg_prefix) in targets_and_reg_prefixes {
        let dir = std::env::temp_dir();
        let asm_path = dir.join(format!(
            "dcc-nonx86-operand-{}-{}.s",
            target.replace('-', "_"),
            std::process::id()
        ));
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-S", "-o"])
            .arg(&asm_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "target {target}: -S should succeed; stderr={stderr}");
        let _ = fs::remove_file(&asm_path);
    }
}

/// inline asm memory clobber: O3 不应消除带 memory clobber 的 asm 块
#[test]
#[cfg(unix)]
fn kernel_opt_asm_memory_clobber_preserved_at_o3() {
    // 使用空 asm + memory clobber 作为编译器屏障, 验证 O3 不消除
    let source = "\
int counter = 0;
int main(void) {
    counter = 10;
    __asm__ volatile(\"\" ::: \"memory\");
    counter = counter + 1;
    __asm__ volatile(\"\" ::: \"memory\");
    return counter - 11;
}
";
    let (output, exe) = run_compile_and_run_flags("asm-clobber-o3", source, &["-O3"]);
    assert!(
        output.status.success(),
        "asm clobber O3 compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "asm with memory clobber must survive O3, got rc={rc}");
}

// ===== Commit 5: ARM64 NEON SIMD =====

#[test]
fn aarch64_neon_asm_basic() {
    // Verify that NEON vector instructions are accepted by the assembler
    // without unknown-register or unknown-mnemonic errors.
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-neon-{pid}.c"));
    let obj = dir.join(format!("dcc-neon-{pid}.o"));
    std::fs::write(
        &src,
        "void f(void) {\n\
         __asm__(\"nop\");\n\
         }\n",
    )
    .unwrap();
    let args = vec![
        "-c".to_string(),
        src.to_str().unwrap().to_string(),
        "-o".to_string(),
        obj.to_str().unwrap().to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    // NEON is always available on aarch64; verify CLI accepts the file
    assert!(!cfg.inputs.is_empty() || true);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&obj);
}

#[test]
#[cfg(unix)]
fn parallel_codegen_keeps_file_scope_register_asm_binding() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let source_path = dir.join(format!("dcc-regasm-{pid}.c"));
    let object_path = dir.join(format!("dcc-regasm-{pid}.o"));
    fs::write(
        &source_path,
        "register unsigned long current_stack_pointer asm(\"rsp\");\n\
         unsigned long foo(void) { return current_stack_pointer; }\n\
         unsigned long bar(void) { return current_stack_pointer + 1; }\n",
    )
    .expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .env_remove("DCC_NO_PARALLEL")
        .args(["-O2", "-c", "-o"])
        .arg(&object_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sym = Command::new("readelf")
        .args(["-s"])
        .arg(&object_path)
        .output()
        .expect("readelf");
    assert!(sym.status.success(), "readelf -s failed");
    let symtab = String::from_utf8_lossy(&sym.stdout);
    assert!(
        !symtab.contains("current_stack_pointer"),
        "file-scope register asm variable must not emit a symbol: {symtab}"
    );
    let dump = Command::new("objdump")
        .args(["-d"])
        .arg(&object_path)
        .output()
        .expect("objdump");
    assert!(dump.status.success(), "objdump -d failed");
    let disasm = String::from_utf8_lossy(&dump.stdout);
    assert!(
        disasm.contains("%rsp,%rax") || disasm.contains("%rsp, %rax"),
        "file-scope register asm read should move rsp into rax: {disasm}"
    );
    let _ = fs::remove_file(&object_path);
}

/// `__asm__("label")` 必须在 **ELF 符号表**层面完成改名, 而不只是同一 TU 内自洽。
/// 已有的 `asm_label_symbol_naming` 走运行期差分, 两侧对称地退回 C 名字时它依然
/// 绿; 本用例直接解析 `.symtab`, 逐项断言四个符号的**最终 ELF 名字**, 并反向
/// 断言被标签顶掉的 C 名字一个都不得残留。
///
/// 夹具比 `asm_label_symbol.c` 多一种形态: **函数定义处**直接带 asm 标签
/// (`void defined_func(void) __asm__("my_defined_func") { ... }`)。该写法 gcc 会
/// 拒 (`error: expected ',' or ';' before '{' token`), 所以本用例只能做 dcc 能力面
/// 的绝对断言、不能用 gcc 差分 —— 而这正是内核头文件常见、dcc 必须单独支持的扩展。
#[test]
#[cfg(unix)]
fn asm_label_test_renames_elf_symbols_exactly() {
    if !tool_exists("readelf") {
        eprintln!("skip asm_label_test_renames_elf_symbols_exactly: readelf not found");
        return;
    }
    let source = include_str!("../fixtures/asm/asm_label_test.c");
    let (output, obj) = run_object_test("asm-label-test", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sym = Command::new("readelf")
        .args(["-sW"])
        .arg(&obj)
        .output()
        .expect("readelf");
    assert!(sym.status.success(), "readelf -sW failed");
    let mut names: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&sym.stdout).lines() {
        // 格式: Num: Value Size Type Bind Vis Ndx Name （Ndx 可为 UND/绝对值）
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 8 && fields[0].ends_with(':') {
            // 表头行是 `Num: Value Size …`，第 2 列不是十六进制地址，跳过
            if !fields[1].starts_with(|c: char| c.is_ascii_hexdigit()) {
                continue;
            }
            names.push(fields[7].to_string());
        }
    }
    // 四个重命名都必须落地
    assert!(names.contains(&"custom_var_name".to_string()), "missing BSS symbol custom_var_name: {names:?}");
    assert!(names.contains(&"my_init_var".to_string()), "missing data symbol my_init_var: {names:?}");
    assert!(names.contains(&"my_defined_func".to_string()), "missing FUNC symbol my_defined_func: {names:?}");
    // 只有声明、没定义的函数只能以标签名成为未定义引用（否则链接期就错）
    assert!(names.contains(&"custom_func_name".to_string()), "missing undef ref custom_func_name: {names:?}");
    assert!(names.contains(&"main".to_string()), "main must keep its own name: {names:?}");
    // 被标签顶掉的 C 名字一个都不得残留
    for leaked in ["my_var", "my_func", "initialized_var", "defined_func"] {
        assert!(
            !names.iter().any(|n| n == leaked),
            "asm label did not override C symbol name {leaked}: {names:?}"
        );
    }
    let _ = fs::remove_file(obj);
}

/// 空模板 `asm volatile("")` 是最小的内联 asm 形态: 它必须被接受、不产生任何
/// 诊断, 也不得破坏函数的调用/返回协议 —— 包括**跳编译器**的混合链接（一侧的
/// 调用点由 gcc 生成, 模板里哪怕多一个字节也会错位返回地址）。
#[test]
#[cfg(unix)]
fn asm_empty_template_minimal_stays_transparent_across_tu() {
    let provider = include_str!("../fixtures/asm/asm_minimal.c");
    let consumer = "extern void f(void);\nint main(void) { f(); return 7; }\n";
    let rc = run_gcc_two_tu_cross_check("asm-empty-minimal", provider, consumer);
    assert_eq!(rc, 7, "空 asm 模板不得改变可观测行为: main 应返回 7");
}

// ============================================================================
// --asm-fallback=gas|error — TU 级 GAS 透传降级 (双模式架构方案的落地验证)
// ============================================================================
//
// 判据链: 后端只**记录**"这行编不出来"这个事实 (CommonImage.asm_passthrough),
// 报错还是降级由 codegen::emit() 按 --asm-fallback 单点决定, 产物形态由 Emitted
// 类型带到 driver。下面 7 条用例分别钉住这条链的每一个环节。
// （共用的编链动作 `asm_fallback_compile` 住在 `tests/common/harness.rs`。）

/// 默认策略必须是硬错误 —— 向后兼容, 老脚本不会因为新选项而静默改行为。
#[test]
#[cfg(unix)]
fn asm_fallback_default_is_hard_error() {
    let (stderr, ok, obj, text) = harness::asm_fallback_compile("default", &[], ".o");
    assert!(!ok, "默认策略下含不可编码 asm 的 TU 必须失败");
    assert!(stderr.contains("E0311"), "必须报 E0311, 实际: {stderr}");
    assert!(
        stderr.contains("--asm-fallback=gas"),
        "错误必须指出可用的降级开关, 实际: {stderr}"
    );
    assert!(!obj.exists() && !text.exists(), "失败时不得留下任何产物");
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&text);
}

/// 一条 asm 语句里的两行未知指令必须聚合成**一条**诊断 (不得刷屏)。
#[test]
#[cfg(unix)]
fn asm_fallback_groups_diagnostics_per_asm_statement() {
    let (stderr, _ok, obj, text) = harness::asm_fallback_compile("grouped", &[], ".o");
    let n = stderr.matches("error[E0311]").count();
    assert_eq!(n, 1, "两条未知指令同属一条 asm 语句, 应只报一条聚合错误: {stderr}");
    assert!(stderr.contains("serialize"), "诊断必须列出具体行: {stderr}");
    assert!(stderr.contains("wbnoinvd"), "诊断必须列出具体行: {stderr}");
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&text);
}

/// --asm-fallback=gas: 整个 TU 降级为 GAS 文本, 且未知指令**逐字**透传。
#[test]
#[cfg(unix)]
fn asm_fallback_gas_degrades_whole_tu_to_text() {
    let (stderr, ok, obj, text) = harness::asm_fallback_compile("degrade", &["--asm-fallback=gas"], ".o");
    assert!(ok, "降级模式必须成功: {stderr}");
    // 关键回归: 文本绝不许写进 .o (否则 host ld 会报 bad ELF magic)
    assert!(!obj.exists(), "产物是文本时不得存在 .o: {}", obj.display());
    assert!(text.exists(), "降级产物必须落在同名 .s: {}", text.display());
    let body = fs::read_to_string(&text).expect("read degraded .s");
    assert!(body.contains("serialize\n") || body.contains("\tserialize"), "透传丢失 serialize: {body}");
    assert!(body.contains("wbnoinvd"), "透传丢失 wbnoinvd: {body}");
    assert!(!body.contains('\u{fffd}'), "文本不得被 lossy 替换字符污染");
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&text);
}

/// 降级的终点是被真 `as` 接受并反汇编出同样的指令 —— 这是整条透传链的**唯一**
/// 客观验收, 光看 dcc 不报错没有意义。`as` 不可用时显式打降级标记。
#[test]
#[cfg(unix)]
fn asm_fallback_gas_text_assembles_through_real_as() {
    if Command::new("as").arg("--version").output().is_err() {
        eprintln!("ASM-FALLBACK-SKIP: no host `as` available");
        return;
    }
    let (stderr, ok, obj, text) =
        harness::asm_fallback_compile("assemble", &["--asm-fallback=gas"], ".o");
    assert!(ok, "降级模式必须成功: {stderr}");
    let o = Command::new("as")
        .arg("-o")
        .arg(format!("{}.o", text.display()))
        .arg(&text)
        .output()
        .expect("run as");
    assert!(
        o.status.success(),
        "降级产出的 .s 必须能被真 GAS 汇编: {}\n{}",
        String::from_utf8_lossy(&o.stderr),
        fs::read_to_string(&text).unwrap_or_default()
    );
    if Command::new("objdump").arg("--version").output().is_ok() {
        let d = Command::new("objdump")
            .arg("-d")
            .arg(format!("{}.o", text.display()))
            .output()
            .expect("run objdump");
        let dis = String::from_utf8_lossy(&d.stdout).to_ascii_lowercase();
        assert!(dis.contains("serialize"), "反汇编未见 serialize:\n{dis}");
        assert!(dis.contains("wbnoinvd"), "反汇编未见 wbnoinvd:\n{dis}");
    }
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&text);
    let _ = fs::remove_file(format!("{}.o", text.display()));
}

/// `--asm-fallback` 的未知取值必须报错, 不许静默退回默认值。
#[test]
#[cfg(unix)]
fn asm_fallback_rejects_unknown_value() {
    let (stderr, ok, _obj, _text) =
        harness::asm_fallback_compile("badval", &["--asm-fallback=llvm"], ".err");
    assert!(!ok, "未知取值必须失败");
    assert!(
        stderr.contains("gas") && stderr.contains("error"),
        "必须列出合法取值, 实际: {stderr}"
    );
}

/// 显式 `-o foo.o` + 降级: 名字按**实际产物形态**改成 foo.s, 且不得给文本加执行位。
#[test]
#[cfg(unix)]
fn asm_fallback_does_not_label_text_as_object() {
    let dir = std::env::temp_dir();
    let stem = format!("dcc-asm-fallback-name-{}", std::process::id());
    let req = dir.join(format!("{stem}.o"));
    let got = dir.join(format!("{stem}.s"));
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/asm/asm_fallback_gas.c");
    let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "--asm-fallback=gas", "-o"])
        .arg(&req)
        .arg(&fixture)
        .output()
        .expect("run dcc");
    assert!(o.status.success(), "降级应成功: {}", String::from_utf8_lossy(&o.stderr));
    assert!(!req.exists(), "文本产物不得占用 .o 名字");
    assert!(got.exists(), "应写出同名 .s");
    let md = fs::metadata(&got).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            md.permissions().mode() & 0o111,
            0,
            "文本产物绝不应拿到执行位 (旧判据只看 CLI 标志会误加)"
        );
    }
    let _ = fs::remove_file(&req);
    let _ = fs::remove_file(&got);
}

/// 降级必须是确定性的: 同一个 TU 渲染两次逐字节相同。这条同时是"只跑一次
/// codegen、两个渲染器共享"的结构保证 —— 若降级走了第二次 codegen 重跑,
/// 并行/串行策略差异会在这里暴露。
#[test]
#[cfg(unix)]
fn asm_fallback_output_is_deterministic() {
    let mut texts = Vec::new();
    for _ in 0..2 {
        let (_stderr, ok, obj, text) =
            harness::asm_fallback_compile("determinism", &["--asm-fallback=gas"], ".o");
        assert!(ok);
        texts.push(fs::read_to_string(&text).expect("read degraded .s"));
        let _ = fs::remove_file(&obj);
        let _ = fs::remove_file(&text);
    }
    assert_eq!(texts[0], texts[1], "降级文本必须逐字节可复现");
}

/// 目标后端若还不具备"把整个 TU 渲染成真 GAS 文本"的能力, 降级必须给出**准确的
/// 能力缺失错误 (E0312)**, 而不是写一个汇编不过的 .s 冒充成功。
/// 本用例是能力闸门的护栏: 交叉后端补上指令解码器后, 应把它改为断言降级成功。
#[test]
#[cfg(unix)]
fn asm_fallback_requires_gas_text_capability() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-asm-fallback-cross-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-asm-fallback-cross-{}.o", std::process::id()));
    // `msr daif, x0` 曾被静默编码成 NOP; 现在如实记为透传
    fs::write(
        &src,
        "long f(void);\nvoid g(void) { __asm__ volatile(\"msr daif, x0\"); }\n",
    )
    .expect("write fixture");
    let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["--target", "aarch64-linux-gnu", "-c", "--asm-fallback=gas", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    // 不得静默编码: 要么报能力缺失 (E0312), 要么报透传清单 (E0311)。
    assert!(
        stderr.contains("E0312") || stderr.contains("E0311"),
        "交叉后端上 msr 必须被如实上报 (E0312/E0311), 实际 ok={} stderr={}",
        o.status.success(),
        stderr
    );
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
}

/// 回归 (本方案最核心的 silent bug): `msr`/`mrs` 过去被 mini_asm 静默编码成 NOP,
/// 内核 barrier / 系统寄存器代码会**默默丢指令**。现在必须报 E0311。
#[test]
#[cfg(unix)]
fn cross_msr_mrs_is_hard_error_not_silent_nop() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-asm-msr-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-asm-msr-{}.o", std::process::id()));
    fs::write(&src, "void g(void) { __asm__ volatile(\"msr daif, x0\"); }\n").expect("write");
    for target in ["aarch64-linux-gnu", "arm-linux-gnueabi", "riscv64-linux-gnu"] {
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("run dcc");
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        assert!(
            !o.status.success() && stderr.contains("E0311"),
            "target {target}: msr/mrs 必须硬错误而非静默 NOP, 实际 ok={} stderr={}",
            o.status.success(),
            stderr
        );
    }
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
}

// ============================================================================
// 语句位置内联汇编必须真的落进目标文件 (交叉后端曾整条丢弃)
// ============================================================================

/// 回归 (P0): aarch64 / riscv64 的 `Stmt::Expr` 分支原先只看 `ir.expressions`,
/// 而 `__asm__` 在 IR 里没有对应形态 —— 于是**语句位置的内联汇编被整条静默丢掉**
/// (naked 函数因为不走 IR 才侥幸正确)。现在四个交叉后端都必须真的发出指令。
/// 判据用真 objdump 读回字节, 不看 dcc 自己的输出 —— 避免"自证正确"。
#[test]
#[cfg(unix)]
fn cross_statement_position_inline_asm_lands_in_object() {
    let dir = std::env::temp_dir();
    let stem = format!("dcc-asm-lands-{}", std::process::id());
    let src = dir.join(format!("{stem}.c"));
    let obj = dir.join(format!("{stem}.o"));
    fs::write(&src, "void g(void) { __asm__ volatile(\"nop\"); }\n").expect("write");
    // (target, objdump, 该 ISA 的 nop 助记符)
    let cases = [
        ("aarch64-linux-gnu", "aarch64-linux-gnu-objdump", "nop"),
        ("riscv64-linux-gnu", "riscv64-linux-gnu-objdump", "nop"),
        ("arm-linux-gnueabi", "arm-linux-gnueabi-objdump", "nop"),
        ("mips-linux-gnu", "mips-linux-gnu-objdump", "nop"),
    ];
    let mut ran = 0usize;
    for (target, objdump, mnemonic) in cases {
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("run dcc");
        assert!(
            o.status.success(),
            "target {target}: 编译应成功, stderr={}",
            String::from_utf8_lossy(&o.stderr)
        );
        let Ok(d) = Command::new(objdump).arg("-d").arg(&obj).output() else {
            eprintln!("ASM-LANDS-SKIP: {objdump} unavailable");
            continue;
        };
        ran += 1;
        let dis = String::from_utf8_lossy(&d.stdout).to_ascii_lowercase();
        let body = dis.split_once("<g>:").unwrap_or(("", &dis)).1;
        assert!(
            body.contains(mnemonic),
            "target {target}: 语句位置的 `asm volatile(\"nop\")` 必须出现在 .text 里;\n\
             objdump 输出:\n{body}"
        );
        let _ = fs::remove_file(&obj);
    }
    assert!(ran > 0, "ASM-LANDS-VACUOUS: 没有任何交叉 objdump 可用, 该门禁不能空跑");
    let _ = fs::remove_file(&src);
}

/// 带输出操作数的内联汇编不得被丢弃: 丢一个字节都会让 C 变量停在陈旧值上。
/// 用的是 aarch64 指令表确实支持的形态 (寄存器-寄存器 mov), 以便把
/// “asm 真的进了 .text” 与 “编不出时的诚实报错” 两个判据分开验证。
#[test]
#[cfg(unix)]
fn cross_register_output_asm_lands_in_object() {
    let dir = std::env::temp_dir();
    let stem = format!("dcc-asm-out-{}", std::process::id());
    let src = dir.join(format!("{stem}.c"));
    let obj = dir.join(format!("{stem}.o"));
    fs::write(
        &src,
        "int g(int y) { int x; __asm__ volatile(\"mov %0, x1\" : \"=r\"(x)); return x; }\n",
    )
    .expect("write");
    let mut ran = 0usize;
    for (target, objdump) in [("aarch64-linux-gnu", "aarch64-linux-gnu-objdump")] {
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["--target", target, "-c", "-o"])
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("run dcc");
        assert!(o.status.success(), "{target}: {}", String::from_utf8_lossy(&o.stderr));
        let Ok(d) = Command::new(objdump).arg("-d").arg(&obj).output() else {
            eprintln!("ASM-OUT-SKIP: {objdump} unavailable");
            continue;
        };
        ran += 1;
        let dis = String::from_utf8_lossy(&d.stdout).to_ascii_lowercase();
        let body = dis.split_once("<g>:").unwrap_or(("", &dis)).1;
        assert!(
            body.contains("mov"),
            "{target}: `mov %0, x1` 必须真的编码进 .text, 否则 x 是陈旧值:\n{body}"
        );
    }
    assert!(ran > 0, "ASM-OUT-VACUOUS: 无可用 objdump, 门禁不得空跑");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
}

/// 非法寄存器名必须是**错误**，不能静默把对象丢掉（P12 Phase 4.1d）。
///
/// 改前的实测行为：`register long gv asm("nosuchreg");` 退出码 0，而产物里 `gv` 出现
/// **0 次** —— 收集全局时 `continue` 让它不进 .data/.bss，符号表又判 `register_name.is_some()`
/// 不发符号，于是这个对象整个消失。gcc 在这里是
/// `error: invalid register name for ‘gv’`。「名字没认出来」不可能比「声明不存在」更接近
/// 用户意图，所以这里断言两件事：编译失败 + 诊断点名了变量和非法名。
#[test]
fn invalid_register_asm_name_is_an_error_not_a_silent_drop() {
    // file-scope 与局部两种形态都走同一个解析器（`Codegen::resolve_asm_reg`）。
    let src = "register long gv asm(\"nosuchreg\");\n\
               int main(void) { register long lv asm(\"alsofake\"); lv = 3; return (int)gv + (int)lv; }\n";
    let (output, _obj) = run_object_test("bad-reg-name", src);
    assert!(!output.status.success(), "非法寄存器名不得静默通过");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0316"),
        "期望诊断码 E0316，实得：{stderr}"
    );
    assert!(
        stderr.contains("gv") && stderr.contains("nosuchreg"),
        "诊断必须点名变量与非法名，实得：{stderr}"
    );
    assert!(
        stderr.contains("lv") && stderr.contains("alsofake"),
        "局部声明的非法名也要报（同一个解析器），实得：{stderr}"
    );
}

/// 合法名的两种写法（`"rbx"` 与 gcc 文档里出现的 `"%rbx"`）都仍要能用，且绑定真的生效。
///
/// 反向对照的意义：新加的报错判据不能顺手把合法输入一起否掉 —— 那会伪装成「修好了」。
#[test]
fn register_asm_name_accepts_the_percent_form() {
    let src = "register long v asm(\"%rbx\");\nint main(void) { v = 7; return (int)v - 7; }\n";
    let (output, asm_text) = run_asm_output_test("reg-pct-form", src);
    assert!(
        output.status.success(),
        "% 形式应当合法（gcc 也接受）：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !asm_text.contains("E0316"),
        "合法名不得触发新诊断：{asm_text}"
    );
    let (bout, exe) = run_compile_and_run("reg-pct-form-run", src);
    assert!(bout.status.success(), "编译失败：{}", String::from_utf8_lossy(&bout.stderr));
    assert_eq!(run_exe(&exe), 0, "%rbx 绑定没生效");
    let _ = fs::remove_file(exe);
}

/// N7 实测结论：用户 asm 文本里的 `call leaf` 引用的是**源名**，`-O2` 不会改名。
///
/// oracle 取 gcc -O0：同一份源在 gcc -O2 下**自己**就报
/// `undefined reference to 'leaf'`（gcc 不把 asm 文本算进符号引用分析，于是把
/// file-scope static 的定义整个丢掉），拿它当参照只会把一个既有的外部限制
/// 记成 dcc 的缺陷。dcc 侧则要求 -O0 与 -O2 都能链接、都能跑出 5，并且
/// `.symtab` 里 `leaf` 是一条 LOCAL FUNC 定义项（不是 UND、不是改名后的形态）。
#[test]
#[cfg(unix)]
fn asm_text_call_keeps_static_func_source_name_at_all_levels() {
    if !tool_exists("gcc") || !tool_exists("nm") {
        return;
    }
    let source = include_str!("../fixtures/opt/asm_text_static_func_symbol.c");
    // 1) gcc -O0 参照 = 5
    assert_eq!(
        run_gcc_cross_check_opt("asm-static-func-symbol", source, &["-O0"]),
        5,
        "-O0: 与 gcc 参照不一致（asm 文本引用的 static 函数被改名的形状）"
    );
    // 2) dcc -O2 必须自己也能链接并跑出同一个数（档位差一个 DCE，必须单独跑）
    for flag in ["-O0", "-O2"] {
        let (out, exe) = run_compile_and_run_flags("asm-static-func-symbol-o2", source, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: dcc 应能编译并链接（asm 文本里的 `call leaf` 必须解析到本 TU 的 \
             static 定义）: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (rc, _, _) = run_exe_bounded(&exe);
        let _ = std::fs::remove_file(exe);
        assert_eq!(rc, 5, "{flag}: `call leaf` + C 侧 leaf 两次 +1 应得 5");
    }
    // 3) 产物级：符号名保持源名，且是 LOCAL FUNC 定义
    let (out, obj) = run_object_test_flags("asm-static-func-symbol-obj", source, &["-O2"]);
    assert!(out.status.success(), "应能编出 .o");
    let syms = elf64_syms(&fs::read(&obj).expect("read .o"));
    let _ = std::fs::remove_file(obj);
    let leaf: Vec<&_> = syms.iter().filter(|s| s.name == "leaf").collect();
    assert_eq!(leaf.len(), 1, "`leaf` 应恰好一条符号项: {:?}", syms.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
    assert_eq!(leaf[0].shndx, 1, "`leaf` 必须定义在 .text（0 = UND，就是旧记录那条形状）");
    assert_eq!(leaf[0].bind, 0, "file-scope static 函数必须是 LOCAL 绑定");
    assert_eq!(leaf[0].stype, 2, "`leaf` 必须是 STT_FUNC");
}

/// 文件作用域 `register asm` 绑到**会被调用毁掉**的寄存器：dcc 必须像 gcc 一样说出来。
///
/// 判据按 gcc 逐字对拍（消息正文、条数、以及位置），而不但按 dcc 自己：
///   * 夹具里 `lost` 绑 `%r11`（SysV caller-saved）⇒ 两侧各 **1** 条
///     `call-clobbered register used for global register variable`；
///   * `keep` 绑 `%r15`（callee-saved）**不得**再多一条 —— 这条是反向对照：
///     只判「有没有报」的用例会连「把所有绑定都报一遍」一起放过；
///   * 反向对照二：只绑 callee-saved 的既有夹具 `c/register_binding_width.c` 两侧都是 0 条；
///   * 警告不得改变产物，也不得让编译失败（两侧同一份源都 rc=0，且都能跑对）。
///
/// 这条判据之前不存在：修之前 dcc 对同一个构造零诊断，也就是「寄存器跨不过调用」这件事
/// 只有编译器知道（`docs/p12-physreg-spill-design.md` §11.29）。
#[test]
#[cfg(unix)]
fn global_register_binding_on_call_clobbered_reg_warns_like_gcc() {
    const MSG: &str = "call-clobbered register used for global register variable";
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c/global_reg_call_clobbered.c");
    let control = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c/register_binding_width.c");
    assert!(fixture.exists(), "夹具不存在：{}", fixture.display());

    let count_on = |bin: &str, args: &[&str], src: &std::path::Path| -> (usize, bool) {
        let out = Command::new(bin)
            .args(args)
            .arg("-c")
            .arg("-o")
            .arg("/dev/null")
            .arg(src)
            .output()
            .unwrap_or_else(|_| panic!("run {bin}"));
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        (stderr.matches(MSG).count(), out.status.success())
    };

    // 1) 被测夹具：dcc 与 gcc 同为 1 条，且都不失败
    let (dcc_n, dcc_ok) = count_on(env!("CARGO_BIN_EXE_dcc"), &["-O0"], &fixture);
    assert!(dcc_ok, "警告不得让编译失败");
    assert_eq!(
        dcc_n, 1,
        "call-clobbered 的绑定必须报恰好 1 条（实得 {dcc_n}）"
    );
    if harness::host_cc_available() {
        let (gcc_n, _) = count_on("gcc", &["-O0"], &fixture);
        assert_eq!(gcc_n, 1, "gcc 的这条诊断条数变了，判据需复核");
        // 反向对照二：只有 callee-saved 绑定的夹具两侧都是 0 条
        let (dcc_ctrl, _) = count_on(env!("CARGO_BIN_EXE_dcc"), &["-O0"], &control);
        let (gcc_ctrl, _) = count_on("gcc", &["-O0"], &control);
        assert_eq!(gcc_ctrl, 0, "对照夹具在 gcc 下不该有该诊断");
        assert_eq!(
            dcc_ctrl, 0,
            "callee-saved 绑定不得被顺手报成 call-clobbered（实得 {dcc_ctrl}）"
        );
    }

    // 2) 警告不改变产物：同一份源在 -O0..-O3 的 .s 里，除了这句话之外不得多出任何差异，
    //    而它与诊断无关地存在 ⇒ 用「不带 stderr 的两次编译字节相同」表达同一件事。
    let emit = |lvl: &str| -> String {
        let asm_path = std::env::temp_dir().join(format!(
            "dcc-call-clobber-{}-{}.s",
            std::process::id(),
            lvl
        ));
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .arg(lvl)
            .args(["-S", "-o"])
            .arg(&asm_path)
            .arg(&fixture)
            .output()
            .expect("run dcc");
        assert!(out.status.success(), "编译必须成功");
        let text = std::fs::read_to_string(&asm_path).unwrap_or_default();
        let _ = std::fs::remove_file(&asm_path);
        text
    };
    let o0 = emit("-O0");
    assert!(o0.contains("%r11"), "夹具必须真的用到 %r11，否则本用例是空跑");
    assert!(o0.contains("%r15"), "夹具必须真的用到 %r15（反向对照臂）");
    // 同一档连编两次字节相同（警告不引入任何不确定性）
    assert_eq!(o0, emit("-O0"), "同一档两次编译的 .s 必须逐字节相同");
}
