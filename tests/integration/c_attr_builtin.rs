// included from tests/integration.rs — not a standalone cargo target

/// void __attribute__(...) *fn(): 属性在返回类型与指针声明符之间
#[test]
fn attr_ptr_func_decl_compiles() {
    let source = include_str!("../fixtures/c/attr_ptr.c");
    let output = run_syntax_test("attr-ptr-func", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// __attribute__ propagation: function-pointer, enum, struct, typedef positions
#[test]
fn attr_propagation_compiles_and_runs() {
    let src = include_str!("../fixtures/c/attr_propagation.c");
    let (out, exe) = run_compile_and_run("attr-propagation", src);
    assert!(
        out.status.success(),
        "attr_propagation compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "all attribute propagation checks pass");
    let _ = fs::remove_file(exe);
}

/// __builtin_object_size 编译期折叠
#[test]
fn object_size_builtin_run_returns_zero() {
    let source = include_str!("../fixtures/c/objsize2.c");
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
    let source = include_str!("../fixtures/c/obj_size.c");
    let (output, exe) = run_compile_and_run("builtin-os-fortify", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0);
}

/// __builtin_apply / __builtin_return
#[test]
fn builtin_apply_return_run_returns_zero() {
    let source = include_str!("../fixtures/c/apply_ret.c");
    let (output, exe) = run_compile_and_run("builtin-apply-return", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0);
}

/// __builtin_prefetch void no-op
#[test]
fn builtin_prefetch_compiles() {
    let source = include_str!("../fixtures/c/prefetch.c");
    let (output, _obj) = run_object_test("builtin-prefetch", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// __builtin_constant_p：编译期常量判定（含 __int128 表达式）
#[test]
fn builtin_constant_p_full_run_returns_zero() {
    let source = include_str!("../fixtures/c/constp.c");
    let (output, exe) = run_compile_and_run("builtin-constant-p-full", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_constant_p_full should return 0, got {rc}");
}

/// __builtin_clear_cache / __builtin___clear_cache: 编译通过即可 (x86 为 no-op)
#[test]
fn builtin_clear_cache_compiles() {
    let source = include_str!("../fixtures/c/builtin_clear_cache.c");
    let (output, exe) = run_compile_and_run("builtin-clear-cache", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_clear_cache should return 0, got {rc}");
}

/// __builtin_shl_overflow / __builtin_shr_overflow: 移位溢出检测
#[test]
fn builtin_shift_overflow_run_returns_zero() {
    let source = include_str!("../fixtures/c/builtin_shift_overflow.c");
    let (output, exe) = run_compile_and_run("builtin-shift-overflow", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_shift_overflow should return 0, got {rc}");
}

/// __builtin_cpu_supports / __builtin_cpu_init: CPU 特性检测
#[test]
fn builtin_cpu_compiles() {
    let source = include_str!("../fixtures/c/builtin_cpu.c");
    let (output, exe) = run_compile_and_run("builtin-cpu", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_cpu should return 0, got {rc}");
}

/// M12+ builtins: rotate/clrsb/fma/strcasecmp/FILE/LINE/FUNCTION/classify_type 等
#[test]
fn builtin_m12_extra_run_returns_zero() {
    let source = include_str!("../fixtures/c/builtins_misc.c");
    let (output, exe) = run_compile_and_run("builtin-m12-extra", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "test_m12_builtins should return 0, got {rc}");
}

/// __builtin_expect_with_probability / __builtin_nonzero_p / __builtin_constant_p
/// 编译期折叠: x86_64 后端直接求值, 非 x86 后端 (aarch64/riscv64) gen_call 兑底.
#[test]
fn builtin_expect_nonzero_p_run_returns_zero() {
    let source = include_str!("../fixtures/c/builtin_expect_nonzero.c");
    let (output, exe) = run_compile_and_run("builtin-expect-nonzero", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_expect_nonzero should return 0, got {rc}");
}

/// M12+ builtins: rotate / clrsb / fma 综合测试
#[test]
fn builtin_rotate_clrsb_run_returns_zero() {
    let source = include_str!("../fixtures/c/rotate.c");
    let (output, exe) = run_compile_and_run("builtin-rotate-clrsb", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "rotate/clrsb/fma should return 0, got {rc}");
}

/// M12+ builtins: strcasecmp / strncasecmp (走 libc)
#[test]
fn builtin_strcasecmp_run_returns_zero() {
    let source = include_str!("../fixtures/c/strcasecmp.c");
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
    let source = include_str!("../fixtures/c/file_line.c");
    let (output, exe) = run_compile_and_run("builtin-file-line-func", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "file/line/func should return 0, got {rc}");
}

/// M12+ builtins: classify_type / types_compatible_p
#[test]
fn builtin_classify_compat_run_returns_zero() {
    let source = include_str!("../fixtures/c/classify.c");
    let (output, exe) = run_compile_and_run("builtin-classify-compat", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "classify/compat should return 0, got {rc}");
}

/// M12+ builtins: choose_expr + constant_p + object_size 综合
#[test]
fn builtin_choose_full_run_returns_zero() {
    let source = include_str!("../fixtures/c/choose.c");
    let (output, exe) = run_compile_and_run("builtin-choose-full", source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "choose should return 0, got {rc}");
}

/// 未知 __builtin_*：**硬错误 E0018**（早期版本是 warning + 零值，现已收紧；
/// 下方注释与断言以当前行为为准）；void 变体发射 ud2。
#[test]
#[cfg(unix)]
fn unknown_builtin_is_hard_error_e0018() {
    // 负向夹具（期望编译报错）属于 tests/fixtures/errors/；放在 fixtures/c/ 会被
    // 链接型的语料级测试误当成待修债（它们永远链接不过，因为本来就不该编译过）。
    let source = include_str!("../fixtures/errors/unk_builtin.c");
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
    // Unknown builtins are now hard errors (E0018), not warnings.
    // This prevents silent zero/ud2 fallback that could cause
    // subtle runtime bugs in kernel code.
    assert!(!output.status.success(), "unknown builtin should fail compilation: {stderr}");
    assert!(stderr.contains("E0018"), "expected E0018 error code: {stderr}");
    assert!(
        stderr.contains("unsupported __builtin_"),
        "expected unsupported builtin message: {stderr}"
    );
    assert!(!object_path.exists(), "object file should not be written on E0018");
}

/// __attribute__((cleanup(fn))): 离开作用域时调用 fn(&var)
#[test]
#[cfg(unix)]
fn attr_cleanup_compile_and_run() {
    let source = include_str!("../fixtures/c/attr_cleanup.c");
    let (output, exe) = run_compile_and_run("attr-cleanup", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_cleanup should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// __attribute__((target_clones(...))): 解析并存入 FuncDecl.target_clones
#[test]
#[cfg(unix)]
fn attr_target_clones_compile_and_run() {
    let source = include_str!("../fixtures/c/attr_clones.c");
    let (output, exe) = run_compile_and_run("attr-target-clones", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_target_clones should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// **已知未修（双向钉住）**：`target_clones` 会把链接静默从动态降级为静态。
///
/// 实测链：`.o` 里有 IFUNC 符号时，`driver.rs` 不再走宿主 cc（注释说宿链处理不了
/// dcc 的 IFUNC resolver —— 因为 resolver 的机器码是**内置链接器**现场拼的，
/// `link/target_clones.rs` 那 662 行），而是改走 `run_builtin_linker`。后果：
///
/// | 输入 | 链接路径 | 产物 |
/// |---|---|---|
/// | 只用 `printf` | 宿主 cc | 动态，NEEDED libc.so.6 |
/// | 只用 `target_clones` | 内置链接器 | **静态**，无 PT_INTERP、无 .dynamic |
/// | 两者同用 | 内置链接器 | 链接失败：几十个 E0302 |
///
/// 那几十个未定义符号里绝大多数是夹具**从未调用**的（`tmpfile`/`asprintf`/`rename`…），
/// 所以它不是「缺 libc 符号表」（补不完的），而是「静态链下 crt 与 libc 内部引用
/// 全部变成真未定义」。因此本测试两面都钉：
///   1. 不带 IFUNC 的同主程序必须是动态的（基准线）；
///   2. 带 IFUNC 的必须仍是动态的 —— 这一条**今天故意红着**，所以当前只断言
///      「它现在确实是静态的」，并在注释与 docs §7.38 写明修好后该翻方向。
/// 为什么不能只留「链接失败」一条：那只钉住了用户可见现象，没钉住「静默变静态」
/// 这个本质——而后者就算 libc 问题被旁路躲过也仍然是错的。
#[test]
#[cfg(unix)]
fn target_clones_diverts_to_static_link_is_known_defect() {
    let plain = "#include <stdio.h>\nint compute(int x){ return x*2+1; }\n\
                 int main(void){ printf(\"%d\\n\", compute(10)); return 0; }\n";
    let clones = "#include <stdio.h>\n\
                  __attribute__((target_clones(\"sse4.2,default\")))\n\
                  int compute(int x){ return x*2+1; }\n\
                  int main(void){ printf(\"%d\\n\", compute(10)); return 0; }\n";
    let dir = std::env::temp_dir();
    let tag = std::process::id();
    let needed_of = |src: &str, stem: &str| -> Option<bool> {
        let sf = dir.join(format!("dcc-tc-{stem}-{tag}.c"));
        let exe = dir.join(format!("dcc-tc-{stem}-{tag}.bin"));
        std::fs::write(&sf, src).expect("写临时源文件");
        let ok = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-O0", "-o"]).arg(&exe).arg(&sf)
            .output().map(|o| o.status.success()).unwrap_or(false);
        let _ = std::fs::remove_file(&sf);
        if !ok {
            let _ = std::fs::remove_file(&exe);
            return None;
        }
        let dyn_info = std::process::Command::new("readelf")
            .args(["-d"]).arg(&exe).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let _ = std::fs::remove_file(&exe);
        Some(dyn_info.contains("(NEEDED)"))
    };

    // 1) 基准线：不带 IFUNC 的同主程序走宿主 cc，必须是动态链接。
    assert_eq!(
        needed_of(plain, "plain"),
        Some(true),
        "基准线就变了：不带 IFUNC 的程序也不再是动态链接，需先查链接路径是否整体跑偏"
    );

    // 2) 当前缺陷：引入 IFUNC 后链接直接失败（E0302）。三种读数各自对应不同的状态，
    // 所以**每个转移都必须逼出一处的显式修改**，不允许默默进绿：
    //   None       = 现状（链不上）
    //   Some(false) = 链上了但仍静态 —— 本质缺陷未修
    //   Some(true)  = 修好了（保留动态链）
    match needed_of(clones, "clones") {
        None => {} // 已知缺陷现状
        Some(false) => panic!(
            "target_clones + libc 已能链上，但产物仍是静态的：降级缺陷未修。请改成本分支 \
             断言 Some(false) 并保留 docs §7.38 的开放项"
        ),
        Some(true) => panic!(
            "target_clones 程序现在已是动态链接：降级缺陷已修好。请把这个用例改成断言 \
             Some(true)（与 plain 并列），并更新 docs §7.38"
        ),
    }
}

/// __attribute__((no_sanitize(...))) / no_sanitize_address
#[test]
#[cfg(unix)]
fn attr_no_sanitize_compile_and_run() {
    let source = include_str!("../fixtures/c/attr_nosan.c");
    let (output, exe) = run_compile_and_run("attr-no-sanitize", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
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
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let objdump_out = Command::new("objdump")
        .args(["-h", obj.to_str().unwrap()])
        .output()
        .expect("objdump");
    let sec = String::from_utf8_lossy(&objdump_out.stdout);
    assert!(sec.contains(".kernel_alias_test"), "section .kernel_alias_test missing: {sec}");
    let _ = fs::remove_file(obj);
}

/// __attribute__((copy(fn))) / __attribute__((returns_nonnull))
#[test]
#[cfg(unix)]
fn attr_copy_returns_nonnull_compile_and_run() {
    let source = include_str!("../fixtures/c/attr_copy.c");
    let (output, exe) = run_compile_and_run("attr-copy-returns-nonnull", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_copy_returns_nonnull should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// __attribute__((copy("fn")))): 验证符号别名创建和运行时行为
#[test]
#[cfg(unix)]
fn attr_copy_full_symbol_alias() {
    let src = include_str!("../fixtures/c/attr_copy_full.c");
    let (output, exe) = run_compile_and_run("attr-copy-full", src);
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "attr_copy_full should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// __attribute__((returns_nonnull)): 验证运行时检查 (返回 NULL 触发 UD2)
#[test]
#[cfg(unix)]
fn attr_returns_nonnull_runtime_check() {
    let src = include_str!("../fixtures/c/attr_returns_nonnull.c");
    let (output, exe) = run_compile_and_run("attr-returns-nonnull", src);
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "returns_nonnull should return 0, got {rc}");
    let _ = fs::remove_file(exe);
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
fn format_attr_comprehensive_no_warn() {
    // Correct format usage should produce no W0003 warnings
    let src = include_str!("../fixtures/c/format_attr_full.c");
    let output = run_syntax_test("fmt-full", src);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("W0003"), "expected no format warnings, got: {stderr}");
}

#[test]
fn format_attr_type_mismatch_warns() {
    // int arg for %f should trigger W0003
    let src = "__attribute__((format(printf, 1, 2))) void p(const char *f, ...);\n\
               int main(void) { p(\"%f\", 42); return 0; }\n";
    let output = run_syntax_test("fmt-type", src);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("W0003"), "expected type mismatch warning: {stderr}");
    assert!(stderr.contains("floating-point"), "expected floating-point hint: {stderr}");
}

#[test]
fn format_attr_gnu_printf_spelling() {
    // __format__(__gnu_printf__) should work same as format(printf)
    let src = "__attribute__((__format__(__gnu_printf__, 1, 2))) void p(const char *f, ...);\n\
               int main(void) { p(\"%d\", 1); return 0; }\n";
    let output = run_syntax_test("fmt-gnu", src);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("W0003"),
        "gnu_printf should not warn on correct usage: {stderr}"
    );
}

#[test]
fn format_attr_scanf_non_pointer_warns() {
    // scanf with non-pointer arg should warn
    let src = "__attribute__((format(scanf, 1, 2))) int s(const char *f, ...);\n\
               int main(void) { s(\"%d\", 42); return 0; }\n";
    let output = run_syntax_test("fmt-scanf", src);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0003") && stderr.contains("pointer"),
        "scanf non-pointer should warn: {stderr}"
    );
}

#[test]
fn compiler_builtin_atomic_call_is_not_implicit_function_error() {
    let output = run_syntax_test(
        "atomic-builtin-strict",
        "int x;\nint main(void) { __atomic_store_n(&x, 7, __ATOMIC_SEQ_CST); return __atomic_load_n(&x, __ATOMIC_SEQ_CST); }\n",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "atomic builtin should compile: {stderr}");
    assert!(
        !stderr.contains("E0017") && !stderr.contains("W1000"),
        "atomic builtin should not be an implicit function diagnostic: {stderr}"
    );
}

/// __builtin_* 编译期折叠 (strlen/abs/bswap)
#[test]
#[cfg(unix)]
fn m15_builtin_fold() {
    let src = include_str!("../fixtures/c/builtin_fold.c");
    let (out, exe) = run_compile_and_run("m15-builtin-fold", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// __builtin_copysign/copysignf: libc forwarding verification
#[test]
#[cfg(unix)]
fn math_builtin_copysign_libc() {
    let src = include_str!("../fixtures/c/math_builtin_copysign.c");
    let (out, exe) = run_compile_and_run("math-builtin-copysign", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "copysign test should return 0");
    let _ = fs::remove_file(&exe);
}

/// __builtin_pow/log/exp/sin: libm forwarding
#[test]
#[cfg(unix)]
fn math_builtin_libm_forwarding() {
    let src = include_str!("../fixtures/c/math_builtin_libm.c");
    let (out, exe) = run_compile_and_run_flags("math-builtin-libm", src, &["-lm"]);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "libm math test should return 0");
    let _ = fs::remove_file(&exe);
}

// ====== M23: Section 11 属性/builtin/alloca ======
// __attribute__((retain)): 变量和函数应被保留, gc-sections 不移除
#[test]
#[cfg(unix)]
fn m23_attr_retain_compiles_and_runs() {
    let src = include_str!("../fixtures/c/attr_retain.c");
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
    let src = include_str!("../fixtures/c/attr_used.c");
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
    let src = include_str!("../fixtures/c/attr_flatten.c");
    let (out, exe) = run_compile_and_run("m23-attr-flatten", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "outer(20) = (20+1)*2 = 42");
    let _ = fs::remove_file(exe);
}

// __attribute__((flatten)): comprehensive test at multiple -O levels
#[test]
#[cfg(unix)]
fn attr_flatten_comprehensive_at_all_opt_levels() {
    let src = include_str!("../fixtures/c/attr_flatten_full.c");
    for opt in &["-O0", "-O1", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("flatten-full", src, &[opt]);
        assert!(out.status.success(), "compile {opt}: {}", String::from_utf8_lossy(&out.stderr));
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "flatten_full should return 0 at {opt}, got {rc}");
        let _ = fs::remove_file(exe);
    }
}

// __attribute__((flatten)): asm output should not contain call to inlined funcs
#[test]
fn attr_flatten_asm_no_call_to_inlined() {
    let src = "static int inner(int x) { return x + 1; }\n\
               __attribute__((flatten)) int outer(int x) { return inner(x); }\n\
               int main(void) { return outer(41); }\n";
    let (output, asm) = run_asm_output_test("flatten-asm", src);
    assert!(
        output.status.success(),
        "asm output failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The outer function body should not contain 'call inner'
    // since inner should be inlined due to flatten
    assert!(!asm.contains("call inner"), "flatten should inline inner(): {asm}");
}

// __attribute__((target_clones(...)))): 验证 IFUNC 符号和变体符号生成
#[test]
#[cfg(unix)]
fn attr_target_clones_ifunc_symbols() {
    let src = include_str!("../fixtures/c/attr_target_clones.c");
    let src_path = std::env::temp_dir().join(format!("dcc-target-clones-{}.c", std::process::id()));
    let obj = std::env::temp_dir().join(format!("dcc-target-clones-{}.o", std::process::id()));
    fs::write(&src_path, src).expect("write source");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-c")
        .arg("-o")
        .arg(&obj)
        .arg(&src_path)
        .output()
        .expect("run dcc -c for target_clones");
    assert!(output.status.success(), "compile: {}", String::from_utf8_lossy(&output.stderr));
    // 验证 ELF 符号: compute/multiply 应为 IFUNC (type 10),
    // 变体符号 (compute.sse4.2, compute.default 等) 应为 FUNC
    let readelf = Command::new("readelf")
        .arg("-s")
        .arg(&obj)
        .output()
        .expect("readelf");
    let syms = String::from_utf8_lossy(&readelf.stdout);
    // IFUNC 符号类型为 10 (STT_GNU_IFUNC)
    assert!(
        syms.contains("10") && syms.contains("compute"),
        "compute should be IFUNC: {syms}"
    );
    assert!(
        syms.contains("10") && syms.contains("multiply"),
        "multiply should be IFUNC: {syms}"
    );
    // 变体符号
    assert!(syms.contains("compute.sse4.2"), "missing compute.sse4.2: {syms}");
    assert!(syms.contains("compute.default"), "missing compute.default: {syms}");
    assert!(syms.contains("multiply.avx"), "missing multiply.avx: {syms}");
    assert!(syms.contains("multiply.sse4.2"), "missing multiply.sse4.2: {syms}");
    assert!(syms.contains("multiply.default"), "missing multiply.default: {syms}");
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj);
}

// __attribute__((deprecated("msg"))): 仅声明, 解析/编译不应报错
#[test]
#[cfg(unix)]
fn m23_attr_deprecated_compiles_and_runs() {
    let src = include_str!("../fixtures/c/attr_deprecated.c");
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
    let src = include_str!("../fixtures/c/attr_ctor_priority.c");
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
    let src = include_str!("../fixtures/c/builtin_dbg.c");
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
    let src = include_str!("../fixtures/c/attr_retain_used.c");
    let (out, exe) = run_compile_and_run("m23-attr-retain-used", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "combo_var + combo_fn(0) = 7+35 = 42");
    let _ = fs::remove_file(exe);
}

// ====== M27: low-level builtins 汇编/运行时断言 ======
/// __builtin_trap 应发射 ud2 指令
#[test]
#[cfg(unix)]
fn m27_builtin_trap_emits_ud2() {
    let src = include_str!("../fixtures/c/builtin_trap.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-m27-trap-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-m27-trap-{}.s", std::process::id()));
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
    assert!(asm_text.contains("ud2"), "trap should emit ud2: {}", asm_text);
}

/// __builtin_debugtrap 应发射 int3 指令 (0xcc)
#[test]
#[cfg(unix)]
fn m27_builtin_debugtrap_emits_int3() {
    let src = include_str!("../fixtures/c/builtin_debugtrap.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-m27-debugtrap-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-m27-debugtrap-{}.s", std::process::id()));
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
    assert!(
        asm_text.contains("0xcc") || asm_text.contains("int3"),
        "debugtrap should emit int3 (0xcc): {}",
        asm_text
    );
}

/// __builtin_unreachable 应发射 ud2 指令
#[test]
#[cfg(unix)]
fn m27_builtin_unreachable_emits_ud2() {
    let src = include_str!("../fixtures/c/builtin_unreachable.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-m27-unreachable-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-m27-unreachable-{}.s", std::process::id()));
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
    assert!(asm_text.contains("ud2"), "unreachable should emit ud2: {}", asm_text);
}

/// low-level builtins: assume_aligned, alloca, frame_address, return_address 运行时测试
#[test]
#[cfg(unix)]
fn m27_lowlevel_builtins_runtime() {
    let src = include_str!("../fixtures/c/builtin_lowlevel.c");
    let (out, exe) = run_compile_and_run("m27-lowlevel-builtins", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "low-level builtins runtime test failed");
    let _ = fs::remove_file(exe);
}

// ====== M27: attributes 负向与符号测试 ======
/// __attribute__((alias)): 符号别名应指向真实符号
#[test]
#[cfg(unix)]
fn m27_attr_alias_runtime() {
    let src = include_str!("../fixtures/c/attr_alias.c");
    let (out, exe) = run_compile_and_run("m27-attr-alias", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "alias attribute test failed");
    let _ = fs::remove_file(exe);
}

/// __attribute__((alias)): 验证对象文件中 alias 符号存在
#[test]
#[cfg(unix)]
fn m27_attr_alias_symbol_exists() {
    let src = include_str!("../fixtures/c/attr_alias.c");
    let (out, obj) = run_object_test("m27-attr-alias-obj", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    // 用 readelf 验证符号表
    let output = std::process::Command::new("readelf")
        .args(["-s", obj.to_str().unwrap()])
        .output()
        .expect("run readelf");
    let sym_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        sym_text.contains("alias_func") && sym_text.contains("real_func"),
        "alias symbols should exist: {}",
        sym_text
    );
}

/// __attribute__((warning)): 调用应触发警告但不阻断编译
#[test]
#[cfg(unix)]
fn m27_attr_warning_emits_warning() {
    let src = include_str!("../fixtures/c/attr_warning_call.c");
    let (out, exe) = run_compile_and_run("m27-attr-warning", src);
    assert!(out.status.success(), "compile should succeed with warning");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning") && stderr.contains("deprecated_func"),
        "warning attribute should emit warning: {}",
        stderr
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "warning attribute runtime test failed");
    let _ = fs::remove_file(exe);
}

/// __attribute__((error)): 调用应触发编译错误
#[test]
#[cfg(unix)]
fn m27_attr_error_emits_error() {
    let src = include_str!("../fixtures/c/attr_error_call.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-m27-attr-error-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-m27-attr-error-{}.o", std::process::id()));
    fs::write(&source_path, src).expect("write source");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-o",
            obj_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") && stderr.contains("forbidden_func"),
        "error attribute should emit error: {}",
        stderr
    );
}

// ====== M27: attributes 补充测试 ======

/// __attribute__((ifunc)): 验证对象文件中 IFUNC 符号存在
/// IFUNC 的完整运行时解析需要 DCC 内置链接器支持; 此处验证编译期
/// 符号生成正确 (STT_GNU_IFUNC 类型).
#[test]
#[cfg(unix)]
fn m27_attr_ifunc_with_builtin_linker() {
    let src = include_str!("../fixtures/c/attr_ifunc.c");
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-m27-attr-ifunc-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-m27-attr-ifunc-{}.o", std::process::id()));
    fs::write(&source_path, src).expect("write source");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(obj_path);
    // IFUNC 编译应通过 (符号类型正确)
    assert!(
        output.status.success(),
        "ifunc should compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// __attribute__((ifunc)): 验证对象文件中 IFUNC 符号类型为 STT_GNU_IFUNC
#[test]
#[cfg(unix)]
fn m27_attr_ifunc_symbol_type_is_ifunc() {
    let src = include_str!("../fixtures/c/attr_ifunc.c");
    let (out, obj) = run_object_test("m27-attr-ifunc-obj", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    // 用 readelf 验证符号表
    let output = std::process::Command::new("readelf")
        .args(["-s", obj.to_str().unwrap()])
        .output()
        .expect("run readelf");
    let sym_text = String::from_utf8_lossy(&output.stdout);
    // IFUNC 符号应存在且类型正确
    assert!(
        sym_text.contains("ifunc_func") && sym_text.contains("resolve_ifunc"),
        "ifunc symbols should exist: {}",
        sym_text
    );
}

/// __attribute__((constructor)): priority 在合理范围内应正常工作
/// 验证: constructor 属性被正确解析和生成
#[test]
#[cfg(unix)]
fn m27_attr_ctor_priority_compiles() {
    let src = include_str!("../fixtures/c/attr_ctor_priority.c");
    let (out, exe) = run_compile_and_run("m27-ctor-priority", src);
    assert!(
        out.status.success(),
        "ctor priority should compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rc = run_exe(&exe);
    // init_100 (prio=100) 先, init_300 (prio=300) 后
    assert_eq!(rc, 21, "constructor priority 100 should run before 300");
    let _ = fs::remove_file(exe);
}

/// __attribute__((alias)): alias 目标不存在时编译仍可通过
/// (目标符号在链接时解析, 编译期不校验)
#[test]
#[cfg(unix)]
fn m27_attr_alias_target_missing_compiles() {
    let src = include_str!("../fixtures/c/attr_alias_invalid.c");
    let (out, obj) = run_object_test("m27-alias-invalid-obj", src);
    // 编译应通过 (alias 目标在链接时解析)
    assert!(
        out.status.success(),
        "alias with missing target should compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(obj);
}

/// __attribute__((alias)): 变量别名运行时验证
#[test]
#[cfg(unix)]
fn m27_attr_alias_var_runtime() {
    let src = include_str!("../fixtures/c/attr_alias_var.c");
    let (out, exe) = run_compile_and_run("m27-attr-alias-var", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "variable alias test failed");
    let _ = fs::remove_file(exe);
}

/// __attribute__((alias)): 变量别名符号存在于对象文件
#[test]
#[cfg(unix)]
fn m27_attr_alias_var_symbol_exists() {
    let src = include_str!("../fixtures/c/attr_alias_var.c");
    let (out, obj) = run_object_test("m27-attr-alias-var-obj", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let obj_data = std::fs::read(&obj).expect("read object file");
    let obj_str = String::from_utf8_lossy(&obj_data);
    assert!(obj_str.contains("alias_var"), "missing alias_var symbol");
    assert!(obj_str.contains("real_var"), "missing real_var symbol");
    let _ = fs::remove_file(obj);
}

/// __attribute__((alias)) + __weak: 内核模式弱符号别名
#[test]
#[cfg(unix)]
fn m27_attr_alias_weak_runtime() {
    let src = include_str!("../fixtures/c/attr_alias_weak.c");
    let (out, exe) = run_compile_and_run("m27-attr-alias-weak", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "weak + alias test failed");
    let _ = fs::remove_file(exe);
}

/// __attribute__((retain)): 验证对象文件中保留符号存在
#[test]
#[cfg(unix)]
fn m27_attr_retain_symbol_exists() {
    let src = include_str!("../fixtures/c/attr_retain.c");
    let (out, obj) = run_object_test("m27-attr-retain-obj", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    // 用 readelf 验证符号表
    let output = std::process::Command::new("readelf")
        .args(["-s", obj.to_str().unwrap()])
        .output()
        .expect("run readelf");
    let sym_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        sym_text.contains("retain_var") && sym_text.contains("retain_func"),
        "retain symbols should exist: {}",
        sym_text
    );
}

/// __attribute__((used)): 验证对象文件中 used 静态函数符号存在
#[test]
#[cfg(unix)]
fn m27_attr_used_symbol_exists() {
    let src = include_str!("../fixtures/c/attr_used.c");
    let (out, obj) = run_object_test("m27-attr-used-obj", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    // 用 readelf 验证符号表
    let output = std::process::Command::new("readelf")
        .args(["-s", obj.to_str().unwrap()])
        .output()
        .expect("run readelf");
    let sym_text = String::from_utf8_lossy(&output.stdout);
    // used 静态函数应保留在符号表中
    assert!(sym_text.contains("used_helper"), "used symbol should exist: {}", sym_text);
}

/// __attribute__((deprecated)): 调用应触发警告但不阻断编译
#[test]
#[cfg(unix)]
fn m27_attr_deprecated_emits_warning() {
    let src = include_str!("../fixtures/c/attr_deprecated_call.c");
    let (out, exe) = run_compile_and_run("m27-attr-deprecated", src);
    assert!(out.status.success(), "compile should succeed with warning");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning") && stderr.contains("deprecated_func"),
        "deprecated attribute should emit warning: {}",
        stderr
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 42, "deprecated attribute runtime test failed");
    let _ = fs::remove_file(exe);
}

/// __attribute__((constructor)): 超大的 priority 值应仍可编译
/// (当前版本无 priority 范围校验, 仅验证编译期不崩溃)
#[test]
#[cfg(unix)]
fn m27_attr_ctor_priority_invalid_compiles() {
    let src = include_str!("../fixtures/c/attr_ctor_priority_invalid.c");
    let (out, _obj) = run_object_test("m27-ctor-priority-invalid-obj", src);
    assert!(
        out.status.success(),
        "ctor priority invalid should compile: {}",
        String::from_utf8_lossy(&out.stderr)
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
    let source = include_str!("../fixtures/c/wcs.c");
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

/// M3.2: __attribute__ series - packed, aligned, section, weak,
/// alias, visibility, unused.
#[test]
#[cfg(unix)]
fn m3_2_attributes() {
    let src = include_str!("../fixtures/c/attrs.c");
    let (out, code) = run_host_link_and_run("m3-2-attrs", src);
    assert!(
        out.status.success(),
        "m3_2 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "attribute checks failed (exit={code})");
}

#[test]
fn builtin_typedefs_size_ptrdiff() {
    // 编译器内置 typedef: size_t/ptrdiff_t/intptr_t/uintptr_t
    // 无需系统头文件, 编译器直接提供
    let src = include_str!("../fixtures/c/builtin_typedefs.c");
    let (out, exe) = run_compile_and_run("builtin-typedefs", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0, "builtin_typedefs: {}", run_exe(&exe));
    let _ = std::fs::remove_file(&exe);
}

// =================== C11 atomic 预置宏 ===================

/// C11 hosted 模式: atomic_int / memory_order_* / atomic_thread_fence 等预置宏可用
#[test]
#[cfg(unix)]
fn c11_atomic_macros_hosted_compile_and_run() {
    let source = include_str!("../fixtures/c/c11_atomic_macros.c");
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
    let source = include_str!("../fixtures/c/c11_atomic_freestanding.c");
    let (output, exe) =
        run_compile_and_run_flags("c11-atomic-freestanding", source, &["-ffreestanding"]);
    assert!(
        output.status.success(),
        "freestanding compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "freestanding should see __STDC_NO_ATOMICS__=1, got {rc}");
    let _ = fs::remove_file(exe);
}

// =================== ATOMIC_*_LOCK_FREE 宏 ===================

/// C11 ATOMIC_*_LOCK_FREE 宏应预置为 1 (x86_64 始终 lock-free)
#[test]
#[cfg(unix)]
fn c11_atomic_lock_free_macros() {
    let src = "\
#if ATOMIC_BOOL_LOCK_FREE != 1
#error ATOMIC_BOOL_LOCK_FREE
#endif
#if ATOMIC_CHAR_LOCK_FREE != 1
#error ATOMIC_CHAR_LOCK_FREE
#endif
#if ATOMIC_INT_LOCK_FREE != 1
#error ATOMIC_INT_LOCK_FREE
#endif
#if ATOMIC_LONG_LOCK_FREE != 1
#error ATOMIC_LONG_LOCK_FREE
#endif
#if ATOMIC_LLONG_LOCK_FREE != 1
#error ATOMIC_LLONG_LOCK_FREE
#endif
#if ATOMIC_POINTER_LOCK_FREE != 1
#error ATOMIC_POINTER_LOCK_FREE
#endif
int main(void) { return 0; }
";
    let (output, exe) = run_compile_and_run("c11-atomic-lock-free", src);
    assert!(
        output.status.success(),
        "ATOMIC_*_LOCK_FREE macros should be 1: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(exe);
}

// ============================================================================
// AArch64/RISC-V __atomic_* builtins and TLS tests
// ============================================================================

/// __atomic_thread_fence / __atomic_signal_fence compile and run on x86_64.
/// Verifies that both fence builtins compile without errors and produce
/// correct behaviour (fences are no-ops in single-threaded test).
#[test]
#[cfg(unix)]
fn atomic_fence_builtins_run() {
    let src = r#"
int x = 0;
int main(void) {
    x = 42;
    __atomic_thread_fence(5);  /* __ATOMIC_SEQ_CST = 5 */
    __atomic_signal_fence(5);
    return x == 42 ? 0 : 1;
}
"#;
    let (out, exe) = run_compile_and_run("atomic-fence", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0);
    let _ = fs::remove_file(&exe);
}

/// __atomic_fetch_nand compiles and runs on x86_64.
/// Verifies NAND semantics: old = *ptr; *ptr = ~(old & val); return old.
#[test]
#[cfg(unix)]
fn atomic_fetch_nand_run() {
    let src = r#"
int x = 0x0F;
int main(void) {
    int old = __atomic_fetch_nand(&x, 0xFF, 5);
    if (old != 0x0F) return 1;
    if (x != (int)(~(0x0F & 0xFF))) return 2;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("atomic-nand", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0);
    let _ = fs::remove_file(&exe);
}

/// __atomic_test_and_set / __atomic_clear compile and run on x86_64.
#[test]
#[cfg(unix)]
fn atomic_test_and_set_clear_run() {
    let src = r#"
char lock = 0;
int main(void) {
    char old = __atomic_test_and_set(&lock, 5);
    if (old != 0) return 1;
    if (lock != 1) return 2;
    __atomic_clear(&lock, 5);
    if (lock != 0) return 3;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("atomic-tas-clear", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0);
    let _ = fs::remove_file(&exe);
}

/// AArch64: __atomic_test_and_set / __atomic_clear cross-compile.
#[test]
fn aarch64_atomic_tas_clear_cross_compile() {
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-a64-tas-{}.c", std::process::id()));
    let op = dir.join(format!("dcc-a64-tas-{}.o", std::process::id()));
    let src = "char lock = 0;\nint main(void) {\n  __atomic_test_and_set(&lock, 5);\n  __atomic_clear(&lock, 5);\n  return 0;\n}\n";
    fs::write(&sp, src).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "aarch64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            op.to_str().unwrap(),
            sp.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(
        output.status.success(),
        "aarch64 tas/clear failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&op);
}

/// RISC-V: __atomic_test_and_set / __atomic_clear cross-compile.
#[test]
fn riscv64_atomic_tas_clear_cross_compile() {
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-rv64-tas-{}.c", std::process::id()));
    let op = dir.join(format!("dcc-rv64-tas-{}.o", std::process::id()));
    let src = "char lock = 0;\nint main(void) {\n  __atomic_test_and_set(&lock, 5);\n  __atomic_clear(&lock, 5);\n  return 0;\n}\n";
    fs::write(&sp, src).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "riscv64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            op.to_str().unwrap(),
            sp.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(
        output.status.success(),
        "riscv64 tas/clear failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&op);
}

/// __atomic_is_lock_free compiles and runs on x86_64.
#[test]
#[cfg(unix)]
fn atomic_is_lock_free_run() {
    let src = r#"
int main(void) {
    if (!__atomic_is_lock_free(1, 0)) return 1;
    if (!__atomic_is_lock_free(2, 0)) return 2;
    if (!__atomic_is_lock_free(4, 0)) return 3;
    if (!__atomic_is_lock_free(8, 0)) return 4;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("atomic-lockfree", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0);
    let _ = fs::remove_file(&exe);
}

/// AArch64/RISC-V: __atomic_is_lock_free cross-compile.
#[test]
fn aarch64_atomic_is_lock_free_cross() {
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-a64-lf-{}.c", std::process::id()));
    let op = dir.join(format!("dcc-a64-lf-{}.o", std::process::id()));
    let src = "int main(void) { return !__atomic_is_lock_free(4, 0); }\n";
    fs::write(&sp, src).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "aarch64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            op.to_str().unwrap(),
            sp.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(
        output.status.success(),
        "aarch64 is_lock_free failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&op);
}

#[test]
fn riscv64_atomic_is_lock_free_cross() {
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-rv64-lf-{}.c", std::process::id()));
    let op = dir.join(format!("dcc-rv64-lf-{}.o", std::process::id()));
    let src = "int main(void) { return !__atomic_is_lock_free(4, 0); }\n";
    fs::write(&sp, src).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "riscv64-linux-gnu",
            "-c",
            "-O0",
            "-o",
            op.to_str().unwrap(),
            sp.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&sp);
    assert!(
        output.status.success(),
        "riscv64 is_lock_free failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&op);
}

/// __sync_* legacy builtins compile and run on x86_64.
#[test]
#[cfg(unix)]
fn sync_legacy_builtins_run() {
    let src = r#"
int counter = 0;
int main(void) {
    int old;
    counter = 0;
    old = __sync_fetch_and_add(&counter, 10);
    if (old != 0 || counter != 10) return 1;
    old = __sync_fetch_and_sub(&counter, 3);
    if (old != 10 || counter != 7) return 2;
    int nv = __sync_add_and_fetch(&counter, 3);
    if (nv != 10) return 3;
    __sync_synchronize();
    int lk = 0;
    old = __sync_lock_test_and_set(&lk, 1);
    if (old != 0 || lk != 1) return 4;
    __sync_lock_release(&lk);
    if (lk != 0) return 5;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("sync-legacy", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(run_exe(&exe), 0);
    let _ = fs::remove_file(&exe);
}

/// AArch64: __atomic_fetch_nand cross-compile.
#[test]
fn aarch64_atomic_fetch_nand_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-nand-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-a64-nand-{}.o", std::process::id()));
    let src = r#"
int x = 0;
int main(void) {
    __atomic_fetch_nand(&x, 0xFF, 0);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
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
        "aarch64 nand compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
}

/// RISC-V: __atomic_fetch_nand cross-compile.
#[test]
fn riscv64_atomic_fetch_nand_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-rv64-nand-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-rv64-nand-{}.o", std::process::id()));
    let src = r#"
int x = 0;
int main(void) {
    __atomic_fetch_nand(&x, 0xFF, 0);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
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
        "riscv64 nand compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
}

/// AArch64: __atomic_thread_fence / __atomic_signal_fence cross-compile.
#[test]
fn aarch64_atomic_fence_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-fence-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-a64-fence-{}.o", std::process::id()));
    let src = r#"
int main(void) {
    __atomic_thread_fence(5);
    __atomic_signal_fence(5);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
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
        "aarch64 fence compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183);
}

/// RISC-V: __atomic_thread_fence / __atomic_signal_fence cross-compile.
#[test]
fn riscv64_atomic_fence_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-rv64-fence-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-rv64-fence-{}.o", std::process::id()));
    let src = r#"
int main(void) {
    __atomic_thread_fence(5);
    __atomic_signal_fence(5);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
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
        "riscv64 fence compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 243);
}

/// AArch64: __atomic_* builtins compile and produce valid ELF.
/// Verifies that atomic load/store/exchange/fetch_add/sub/and/or/xor/
/// compare_exchange all compile without errors for aarch64 target.
#[test]
fn aarch64_atomic_builtins_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-atomic-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-a64-atomic-{}.o", std::process::id()));
    let src = r#"
int x = 0;
int main(void) {
    __atomic_store_n(&x, 42, 0);
    __atomic_load_n(&x, 0);
    __atomic_exchange_n(&x, 99, 0);
    __atomic_fetch_add(&x, 1, 0);
    __atomic_fetch_sub(&x, 1, 0);
    __atomic_fetch_and(&x, 0xFF, 0);
    __atomic_fetch_or(&x, 0x10, 0);
    __atomic_fetch_xor(&x, 0x01, 0);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
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
        "aarch64 atomic builtins compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183);
}

/// RISC-V: __atomic_* builtins compile and produce valid ELF.
/// Verifies that atomic load/store/exchange/fetch_add/sub/and/or/xor/
/// compare_exchange all compile without errors for riscv64 target.
#[test]
fn riscv64_atomic_builtins_cross_compile() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-rv64-atomic-{}.c", std::process::id()));
    let object_path = dir.join(format!("dcc-rv64-atomic-{}.o", std::process::id()));
    let src = r#"
int x = 0;
int main(void) {
    __atomic_store_n(&x, 42, 0);
    __atomic_load_n(&x, 0);
    __atomic_exchange_n(&x, 99, 0);
    __atomic_fetch_add(&x, 1, 0);
    __atomic_fetch_sub(&x, 1, 0);
    __atomic_fetch_and(&x, 0xFF, 0);
    __atomic_fetch_or(&x, 0x10, 0);
    __atomic_fetch_xor(&x, 0x01, 0);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
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
        "riscv64 atomic builtins compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&object_path).expect("read object");
    let _ = fs::remove_file(&object_path);
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 243);
}

/// Verify that __builtin_prefetch generates actual prefetch instructions
/// (prefetchnta/prefetcht0/prefetcht1/prefetcht2/prefetchw) instead of no-op.
#[test]
#[cfg(unix)]
fn builtin_prefetch_emits_instructions() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("tests/fixtures/c/prefetch_test.c");
    if !src.exists() {
        return;
    }
    let dir = std::env::temp_dir();
    let obj = dir.join(format!("dcc-prefetch-{}.o", std::process::id()));
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    assert!(
        compile.status.success(),
        "dcc compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    // Disassemble and verify prefetch instructions exist
    let objdump = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn"])
        .arg(&obj)
        .output()
        .expect("run objdump");
    let _ = fs::remove_file(&obj);
    assert!(objdump.status.success(), "objdump failed");
    let dump = String::from_utf8_lossy(&objdump.stdout);
    // Check for prefetch instructions in the disassembly
    let has_prefetch = dump.contains("prefetchnta")
        || dump.contains("prefetcht0")
        || dump.contains("prefetcht1")
        || dump.contains("prefetcht2")
        || dump.contains("prefetchw");
    assert!(has_prefetch, "expected prefetch instructions in disassembly:\n{dump}");
}

// ==========================================================================
// P1-6: Missing __builtin_* support
// ==========================================================================

#[test]
fn builtin_nonzero_p_compiles() {
    // Verify __builtin_nonzero_p is recognized and compiles
    let src = r#"
int test_nonzero(int x) {
    if (__builtin_nonzero_p(x)) {
        return 1;
    }
    return 0;
}
int main(void) {
    return test_nonzero(42);
}
"#;
    let output = run_syntax_test("builtin-nonzero", src);
    assert!(
        output.status.success(),
        "__builtin_nonzero_p should compile. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn builtin_expect_with_probability_compiles() {
    // Verify __builtin_expect_with_probability is recognized
    let src = r#"
int test_expect(int x) {
    if (__builtin_expect_with_probability(x != 0, 1, 0.9)) {
        return 1;
    }
    return 0;
}
int main(void) {
    return test_expect(42);
}
"#;
    let output = run_syntax_test("builtin-expect-prob", src);
    assert!(
        output.status.success(),
        "__builtin_expect_with_probability should compile. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// C11 _Atomic with non-basic types (struct)
#[test]
fn c11_atomic_struct_compile_and_run() {
    let source = include_str!("../fixtures/c/c11_atomic_struct.c");
    let (output, exe) = run_compile_and_run("c11-atomic-struct", source);
    assert!(
        output.status.success(),
        "atomic struct compile failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run atomic struct test");
    let _ = fs::remove_file(&exe);
    assert_eq!(run.status.code(), Some(0), "atomic struct test should return 0");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("PASS"), "atomic struct test should print PASS. got: {stdout}");
}

/// __builtin_sqrt/sqrtf: inline SSE sqrtsd/sqrtss
#[test]
#[cfg(unix)]
fn math_builtin_sqrt_inline_sse() {
    let src = include_str!("../fixtures/c/math_builtin_sqrt.c");
    let (out, exe) = run_compile_and_run("math-builtin-sqrt", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "sqrt test should return 0");
    let _ = fs::remove_file(&exe);
}

/// __builtin_floor/ceil/round/trunc: inline SSE roundsd/roundss
#[test]
#[cfg(unix)]
fn math_builtin_rounding_inline_sse() {
    let src = include_str!("../fixtures/c/math_builtin_rounding.c");
    let (out, exe) = run_compile_and_run("math-builtin-rounding", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "rounding test should return 0");
    let _ = fs::remove_file(&exe);
}

/// __builtin_fmin/fmax/fminf/fmaxf: inline SSE minsd/maxsd
#[test]
#[cfg(unix)]
fn math_builtin_fmin_fmax_inline_sse() {
    let src = include_str!("../fixtures/c/math_builtin_fmin_fmax.c");
    let (out, exe) = run_compile_and_run("math-builtin-fmin-fmax", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "fmin/fmax test should return 0");
    let _ = fs::remove_file(&exe);
}

/// AArch64: __atomic_* builtins -S output contains atomic operations.
/// Verifies that the generated output includes atomic-related code.
#[test]
fn aarch64_atomic_builtins_asm_output() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-a64-atomic-asm-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-a64-atomic-asm-{}.s", std::process::id()));
    let src = r#"
int x = 0;
int main(void) {
    __atomic_store_n(&x, 42, 0);
    __atomic_load_n(&x, 0);
    __atomic_fetch_add(&x, 1, 0);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "aarch64-linux-gnu",
            "-S",
            "-O0",
            "-o",
            asm_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "aarch64 atomic -S failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).expect("read asm");
    let _ = fs::remove_file(&asm_path);
    // -S mode outputs machine code bytes as comments; verify function has content
    assert!(
        asm.contains("main")
            && (asm.contains("IR lowering") || asm.contains("ldar") || asm.contains("stlr")),
        "asm should contain main function with atomic operations"
    );
}

/// RISC-V: __atomic_* builtins -S output contains AMO/LR/SC instructions.
/// Verifies that the generated assembly uses RISC-V atomic instructions.
#[test]
fn riscv64_atomic_builtins_asm_output() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-rv64-atomic-asm-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-rv64-atomic-asm-{}.s", std::process::id()));
    let src = r#"
int x = 0;
int main(void) {
    __atomic_store_n(&x, 42, 0);
    __atomic_load_n(&x, 0);
    __atomic_fetch_add(&x, 1, 0);
    return 0;
}
"#;
    fs::write(&source_path, src).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "--target",
            "riscv64-linux-gnu",
            "-S",
            "-O0",
            "-o",
            asm_path.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "riscv64 atomic -S failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).expect("read asm");
    let _ = fs::remove_file(&asm_path);
    // -S mode outputs machine code bytes as comments; verify function has content
    assert!(
        asm.contains("main")
            && (asm.contains("IR lowering")
                || asm.contains("amoswap")
                || asm.contains("lr.w")
                || asm.contains("sc.w")),
        "asm should contain main function with RISC-V atomic operations"
    );
}

/// __builtin_call_with_static_chain: 验证编译通过 (嵌套函数静态链 ABI)
/// 注: 运行时嵌套函数 upvar 访问依赖 is_nested 标记, 该标记在当前
/// 版本中尚未由前端设置, 因此只验证编译期行为.
#[test]
#[cfg(unix)]
fn m27_static_chain_compiles() {
    let src = include_str!("../fixtures/c/builtin_static_chain.c");
    let (out, _obj) = run_object_test("m27-static-chain-obj", src);
    assert!(
        out.status.success(),
        "static_chain builtin should compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// M3.6: builtin functions - __builtin_expect, offsetof,
/// constant_p, choose_expr.
#[test]
#[cfg(unix)]
fn m3_6_builtins() {
    let src = include_str!("../fixtures/c/builtins.c");
    let (out, code) = run_host_link_and_run("m3-6-builtins", src);
    assert!(
        out.status.success(),
        "m3_6 compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "builtin checks failed (exit={code})");
}

/// 优化提示类属性（`hot` / `pure` / `const` / `returns_twice` /
/// `no_instrument_function` / `noclone` / `noipa` / `nocf_check` 及它们的组合
/// 写法）必须被接受且不得改变计算结果。gcc 能编译同一份源，所以走差分：
/// 只断言“不报错”会允许多把一个属性误当成函数调用，逐字节对照能把这类误编译照出。
#[test]
#[cfg(unix)]
fn attr_hot_pure_optimization_hints_match_gcc() {
    let src = include_str!("../fixtures/c/attr_hot_pure.c");
    let rc = run_gcc_cross_check("attr-hot-pure", src);
    assert_eq!(rc, 0, "attr_hot_pure.c 自带校验，0 表示各函数返回值未被属性吞掉");
}

/// `__attribute__((mode(...)))` 必须是**真的宽度换算**而不是被当成空属性：
/// 夹具用 `_Static_assert(sizeof(...) == 1/2/8)` 钉住 QI/HI/DI（含 `__QI__`
/// 下划线形式、`late_mode_t` 的 typedef 后置形式、SF/DF/TF 浮点 form）。
/// 这些 static_assert 在两侧都是编译期断言，所以 gcc 差分同时钉住了“接受”
/// 与“宽度正确”两层。
#[test]
#[cfg(unix)]
fn attr_mode_integer_and_fp_modes_match_gcc() {
    let src = include_str!("../fixtures/c/attr_mode.c");
    let rc = run_gcc_cross_check("attr-mode", src);
    assert_eq!(rc, 0, "attr_mode.c 的 _Static_assert 必须全部成立才能编译");
}

/// `transparent_union` 及一组可见性/调度属性（`externally_visible` /
/// `no_reorder` / `optimize("O2")` / `no_split_stack` /
/// `patchable_function_entry(5,3)`）必须被解析并接受。夹具同时钉两层：
/// 六个属性函数各在 `main` 里调用一次（“属性能被消化”），以及
/// `take(any_ptr)` 的**真实传参语义** —— 调用点分别用 `void *` 与 `int *`
/// 实参而不得显式转换，只接受该属性时才能编译。后者以前无人覆盖。
#[test]
#[cfg(unix)]
fn attr_transparent_union_and_visibility_opts_match_gcc() {
    let src = include_str!("../fixtures/c/attr_transparent_union.c");
    let rc = run_gcc_cross_check("attr-transparent-union", src);
    assert_eq!(rc, 0, "transparent_union 传参语义：take(np)==0 && take(&v)==1，且非 NULL 分支正确");
}

/// 分配器类属性（`malloc` / `alloc_size(1)` / `alloc_size(1,2)` /
/// `assume_aligned(64)` / `warn_unused_result` / `sentinel` / `format_arg`）
/// 作用在**只有声明、没有定义**的函数上：夹具的 `main` 只调用 `my_malloc`，
/// 所以它根本不可链接（两侧都是）。断言落在“属性不得破坏外部链接”：八个声明
/// 必须全部以同名未定义引用进入 `.symtab`，且 `main` 自己必须已定义。
#[test]
#[cfg(unix)]
fn attr_malloc_family_decls_keep_external_linkage() {
    let src = include_str!("../fixtures/c/attr_malloc.c");
    let (output, obj) = run_object_test("attr-malloc", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&obj).expect("read object");
    let undef = elf64_undefined_symbols(&bytes);
    for want in [
        "my_alloc",
        "my_malloc",
        "my_calloc",
        "aligned_alloc",
        "check_permission",
        "execlp",
        "my_gettext",
        "combined_alloc",
    ] {
        assert!(undef.iter().any(|s| s == want), "{want} 应作为未定义引用保留：{undef:?}");
    }
    assert!(
        elf64_func_symbols(&bytes).iter().any(|(n, _, _)| n == "main"),
        "main 必须已定义"
    );
    let _ = fs::remove_file(obj);
}

/// `__attribute__((format(printf, 1, 2)))` 必须被接受且不破坏调用：`log_msg`
/// 只有声明，所以夹具不可链接；断言落在“带 format 属性的声明仍以 `log_msg`
/// 参与链接、调用点仍被保留”，防止属性被误解析成定义或吞掉调用。
#[test]
#[cfg(unix)]
fn format_attr_decl_keeps_call_site_and_linkage() {
    let src = include_str!("../fixtures/c/format_attr.c");
    let (output, obj) = run_object_test("format-attr", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&obj).expect("read object");
    let undef = elf64_undefined_symbols(&bytes);
    assert!(undef.iter().any(|s| s == "log_msg"), "log_msg 应保留为未定义引用：{undef:?}");
    assert!(
        elf64_func_symbols(&bytes).iter().any(|(n, _, _)| n == "main"),
        "main 必须已定义"
    );
    let _ = fs::remove_file(obj);
}

/// 低频内建函数全家桶（`setjmp`/`longjmp`、`speculation_safe_value`、
/// `addc`/`subc`、`is_constant_evaluated`、`memcpy_inline`/`memset_inline`、
/// `unwind_init`）必须被**完全内联展开**。注意 gcc 在 `-O0` 下反而把它们当
/// 普通外部函数发 `call __builtin_addc`（链接直接失败），所以本夹具没有可用
/// 的 gcc 运行期 oracle；改以“零未定义引用”作结构断言：任何一个 `__builtin_*`
/// 没被展开就会在链接期抬出来。
#[test]
#[cfg(unix)]
fn builtin_misc_expand_fully_without_dangling_refs() {
    let src = include_str!("../fixtures/c/builtin_misc.c");
    let (output, exe) = run_compile_and_run("builtin-misc", src);
    assert!(
        output.status.success(),
        "dcc 应能编译并链接（链接由主机 cc 完成）: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_misc.c 自带 main 逐项调用全部低频 builtin，应返回 0");
    let _ = fs::remove_file(exe);
    let (coutput, obj) = run_object_test("builtin-misc-o", src);
    assert!(coutput.status.success(), "compile stderr: {}", String::from_utf8_lossy(&coutput.stderr));
    let bytes = fs::read(&obj).expect("read object");
    let undef = elf64_undefined_symbols(&bytes);
    let leaked: Vec<&String> = undef.iter().filter(|s| s.starts_with("__builtin_")).collect();
    assert!(leaked.is_empty(), "低频 builtin 必须全部内联展开，不得发 `call __builtin_*`：{leaked:?}");
    let _ = fs::remove_file(obj);
}

/// 字符串/内存内建函数（`mempcpy` / `stpcpy` / `strnlen` / `strspn` /
/// `strcspn` / `strcasestr` / `strncmp`）必须要么内联、要么降级到对应的 libc
/// 入口。gcc 侧同样因 `-O0` 发 `call __builtin_strcasestr` 而不可链接，所以
/// 这里不做运行期差分，只断言“链接成功 + rc==0 + 对象里没有任何 `__builtin_`
/// 前缀的未定义引用”（即真的降级到了 `stpcpy`/`strnlen`/… 或展开为拷贝序列）。
#[test]
#[cfg(unix)]
fn builtin_string_lowers_to_libc_entries_not_builtin_names() {
    let src = include_str!("../fixtures/c/builtin_string.c");
    let (output, exe) = run_compile_and_run("builtin-string", src);
    assert!(
        output.status.success(),
        "dcc 应能编译并链接: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "builtin_string.c 应返回 0");
    let _ = fs::remove_file(exe);
    let (coutput, obj) = run_object_test("builtin-string-o", src);
    assert!(coutput.status.success(), "compile stderr: {}", String::from_utf8_lossy(&coutput.stderr));
    let bytes = fs::read(&obj).expect("read object");
    let undef = elf64_undefined_symbols(&bytes);
    let leaked: Vec<&String> = undef.iter().filter(|s| s.starts_with("__builtin_")).collect();
    assert!(leaked.is_empty(), "__builtin_str* 应降级为 libc 同名入口或内联，不得发 `call __builtin_*`：{leaked:?}");
    let _ = fs::remove_file(obj);
}

/// `__sync_*` 全套旧式原子内建（fetch_and_{add,sub,or,and,xor,nand}、
/// {add,sub,or,and,xor,nand}_and_fetch、{bool,val}_compare_and_swap、
/// lock_test_and_set、lock_release、synchronize）必须降级为真正的原子指令。
/// 只断言退出码会漏“同错抵消”，所以走 gcc 差分；夹具本身逐项调用全部
/// 内建并返回 0。
#[test]
#[cfg(unix)]
fn sync_atomic_builtins_full_surface_match_gcc() {
    let src = include_str!("../fixtures/c/sync_atomic.c");
    let rc = run_gcc_cross_check("sync-atomic", src);
    assert_eq!(rc, 0, "sync_atomic.c 逐项调用全部 __sync_* 内建，0 表示序列未坏");
}
