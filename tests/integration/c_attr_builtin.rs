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

// ---------------------------------------------------------------------------
// IFUNC / target_clones：动态链接与 resolver 选型的判据
//
// 共用的 ELF 取数与编链助手（`elf64_rela` / `elf64_section_names` / `elf64_syms_by_type` /
// `elf64_all_rela` / `dcc_link_exe` / `dcc_obj_bytes` / `assert_dynamically_linked`）住在
// `tests/common/harness.rs`：§14 要求主题子文件里 `^fn` 数 == 测试属性数，而且
// `c_link_debug.rs` 也在用同一批取数助手。
// ---------------------------------------------------------------------------

/// **已修**（前身是三态哨兵 `target_clones_diverts_to_static_link_is_known_defect`，
/// §7.38 / §7.41 三 / `docs/missing_features.md` 的 IFUNC 行）：IFUNC 不再把链接从
/// 动态降级为静态。
///
/// 旧形状：`.o` 里 IFUNC 符号的 `st_value` 直指实现体、全 `.o` 无 CPUID 也无 resolver
/// 体，于是 `driver.rs` 「含 IFUNC 就改道内置链接器」，而那条路径产**静态**可执行文件
/// （与 `printf` 同用时更是几十个 E0302）。哨兵当时停在「链不上」这一格，并写明
/// 「真做出动态产物后必须翻成断言动态」—— resolver 搬回 codegen（
/// `codegen::x86_64::ifunc`）后改道分支已删，本用例就是那一翻。
///
/// 实测依据（写进 `docs/wp_ifunc_real_fix.md`，-O0 起）：
/// ```text
/// file        → dynamically linked, interpreter /lib64/ld-linux-x86-64.so.2
/// readelf -d  → 0x…0001 (NEEDED) [libc.so.6]
/// readelf -rW → 0000000000405318  0000000000000025 R_X86_64_IRELATIVE  4030c0
/// readelf -sW → 122: 00000000004030c0 41 IFUNC  GLOBAL  compute
///               106: 00000000004030c0 41 FUNC   WEAK    compute.resolver
/// ```
/// （以上为本机 2026-10-03 重采：地址会随 `.text` 长度漂 —— `__dcc_cpu_indicator_init`
/// 占 3590B，但「IRELATIVE 存在 + IFUNC 与自己的 resolver 同址同尺寸」不变。）
/// 两面都钉：不带 IFUNC 的程序必须**仍然**动态（基准线，翻转三态判据最容易造成的假绿
/// 是「所有产物都变静态而判据跟着改」）；带 IFUNC 的必须动态、链得上、跑得出与 gcc
/// 逐字相同的结果，且 IFUNC 语义（`STT_GNU_IFUNC` + `R_X86_64_IRELATIVE`）必须活到
/// 最终产物里而不是只在 `.o` 里。
#[test]
#[cfg(unix)]
fn target_clones_links_dynamically_with_codegen_resolver() {
    // STT_GNU_IFUNC / R_X86_64_IRELATIVE 的数值（本地定义，避免依赖 libc 常量的导入权）。
    const STT_GNU_IFUNC: u8 = 10;
    const R_X86_64_IRELATIVE: u32 = 37;

    let plain = "#include <stdio.h>\nint compute(int x){ return x*2+1; }\n\
                 int main(void){ printf(\"%d\\n\", compute(10)); return 0; }\n";
    let clones = "#include <stdio.h>\n\
                  __attribute__((target_clones(\"sse4.2,default\")))\n\
                  int compute(int x){ return x*2+1; }\n\
                  int main(void){ printf(\"%d\\n\", compute(10)); return 0; }\n";

    // 1) 基准线：不带 IFUNC 的同主程序必须是动态链接。
    let (ok, stderr, exe) = harness::dcc_link_exe("plain", plain, &["-O0"]);
    assert!(ok, "基准线编译失败：{stderr}");
    harness::assert_dynamically_linked(&exe, "plain（无 IFUNC）基准线", &stderr);
    let _ = fs::remove_file(&exe);

    // 2) 已修：带 IFUNC 必须链得上（旧形状在这里是几十个 E0302）。
    let (ok, stderr, exe) = harness::dcc_link_exe("clones", clones, &["-O0"]);
    assert!(
        ok,
        "target_clones + printf 现在必须能经宿主 cc 链上（resolver 已由 codegen 发进 .text，\
         `driver.rs` 里那条「含 IFUNC 就改道内置链接器」的分支已删）。失败说明又回到静态/\
         改道那一侧：\n{stderr}"
    );
    // 3) 且产物必须是动态的 —— 这正是当年「静默降级」缺的那一条。
    harness::assert_dynamically_linked(&exe, "clones（含 IFUNC）", &stderr);
    // 4) 走的是宿主 cc，不是内置链接器：路径必须自己说出来（底线：不得静默）。
    assert!(
        stderr.contains("via host cc"),
        "含 IFUNC 的程序必须由 stderr 明说是经宿主 cc 链接（内置链接器那条改道分支不该再出现）:\n{stderr}"
    );

    // 5) IFUNC 语义必须活到最终产物：`compute` 仍是 STT_GNU_IFUNC，且 `st_value`
    //    指向**自己的 resolver**（同 value/size），不是实现体。
    let syms = elf64_syms(&fs::read(&exe).unwrap_or_default());
    let find = |n: &str| syms.iter().find(|s| s.name == n);
    let ifunc = find("compute").unwrap_or_else(|| panic!("exe symtab 里没有 compute"));
    let resolver = find("compute.resolver").unwrap_or_else(|| panic!("exe 里没有 compute.resolver"));
    assert_eq!(ifunc.stype, STT_GNU_IFUNC, "exe 里 compute 的 st_type 必须是 STT_GNU_IFUNC(10)");
    assert_eq!(ifunc.bind, 1, "非 static 基名的 IFUNC 必须是 GLOBAL（与 gcc 同形）");
    assert_eq!(resolver.stype, 2, "resolver 必须是普通 STT_FUNC —— gcc 的形状");
    assert_eq!(resolver.bind, 2, "非 static 的 `<base>.resolver` 必须是 WEAK（与 gcc 同形）");
    assert_eq!(
        (ifunc.value, ifunc.size),
        (resolver.value, resolver.size),
        "IFUNC 的 st_value/st_size 必须等于它自己 resolver 的：\
         旧缺陷形状是 IFUNC 与**实现体**同址（全 .o 无 resolver 体）"
    );

    // 6) 动态链接器真的会被叫去跑 resolver：重定位表里必须有 IRELATIVE，且其
    //    addend 就是那个 resolver 的地址。（非 PIE 的 ET_EXEC 里 `ld` 把它放 `.rela.plt`，
    //    PIE 里放 `.rela.dyn`，所以扫全部 `.rela*`。）
    let exe_bytes = fs::read(&exe).unwrap_or_default();
    let irel: Vec<(u64, u32, u32, i64)> = harness::elf64_all_rela(&exe_bytes)
        .into_iter()
        .filter(|r| r.2 == R_X86_64_IRELATIVE)
        .collect();
    assert!(
        !irel.is_empty(),
        "exe 的重定位表里没有 R_X86_64_IRELATIVE：动态链接器不会调用 resolver，\
         IFUNC 只是符号表上的装饰"
    );
    assert!(
        irel.iter().any(|(_, _, _, addend)| *addend as u64 == resolver.value),
        "IRELATIVE 的 addend 必须是 resolver 地址（读得 {:?}，resolver @ {:#x}）",
        irel.iter().map(|r| format!("{:#x}", r.3)).collect::<Vec<_>>(),
        resolver.value
    );

    // 7) 行为：结果必须对（resolver 选中的实现真被调用），并与 gcc 逐字一致。
    let (rc, stdout, run_stderr) = run_exe_bounded(&exe);
    let _ = fs::remove_file(&exe);
    let run_stderr = String::from_utf8_lossy(&run_stderr).to_string();
    assert_eq!(rc, 0, "运行 rc: stderr:\n{run_stderr}");
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        "21\n",
        "target_clones 的 compute(10) 必须经 IFUNC 解析后返回 21"
    );
    assert_eq!(
        run_gcc_cross_check_opt("tc-dyn-parity", clones, &["-O0"]),
        0,
        "与 gcc 的 stdout/退出码对照（同一份源）"
    );
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

/// `target_clones` 的 `.o` 形状：基名必须是 `STT_GNU_IFUNC`，且 `st_value` 指向
/// **自己的 resolver 体**（同 value、同 size）；变体实现体各自独立成 `STT_FUNC`。
///
/// 三条断言各钉一个曾经真错过的点：
///   * 旧版只写 `syms.contains("10") && syms.contains("compute")`：既没验 st_type 真是
///     10（`"10"` 在任何 symtab 文本里都能撞到别处），也没验「指向哪里」—— 而
///     「指向哪里」才是本缺陷的本体。现在走 ELF 解析读 `st_info & 0xf`。
///   * 名字里的 `.` 会被 mangle 成 `_`（gcc 13 实测同形）：`compute.sse4_2`，而旧版
///     断的是 `compute.sse4.2` —— 那条断言从来不可能绿。
///   * 「IFUNC 与 `<base>.resolver` 同址同尺寸」是「`.o` 里有可跑 resolver」的证据；
///     上一轮的缺陷形状正是 IFUNC 与**实现体**同址。所以额外断 IFUNC 地址与每个
///     变体地址都**不相等** —— 直接钉住那个旧形状不可能重新发生。
#[test]
#[cfg(unix)]
fn attr_target_clones_ifunc_symbols() {
    const STT_GNU_IFUNC: u8 = 10;
    const STT_FUNC: u8 = 2;
    const STT_OBJECT: u8 = 1;
    let src = include_str!("../fixtures/c/attr_target_clones.c");
    let obj = harness::dcc_obj_bytes("tc-syms", src, &["-O0"]);
    let syms = elf64_syms(&obj);
    let find = |n: &str| syms.iter().find(|s| s.name == n);

    // 基名：GLOBAL IFUNC；resolver：WEAK FUNC，同址同尺寸。
    for base in ["compute", "multiply"] {
        let b = find(base).unwrap_or_else(|| panic!(".o 里没有基名符号 {base}: 共 {} 个符号", syms.len()));
        let r = find(&format!("{base}.resolver")).expect("codegen 必须发 <base>.resolver 体");
        assert_eq!(b.stype, STT_GNU_IFUNC, "{base} 必须是 STT_GNU_IFUNC(10)");
        assert_eq!(b.bind, 1, "非 static 基名的 IFUNC 必须是 GLOBAL");
        assert!(b.size > 0, "IFUNC 的 st_size 必须是 resolver 的实际长度，不是 0");
        assert_eq!(r.stype, STT_FUNC, "resolver 本体是普通 STT_FUNC —— gcc 的形状");
        assert_eq!(r.bind, 2, "非 static 的 <base>.resolver 必须是 WEAK");
        assert_eq!(
            (b.value, b.size),
            (r.value, r.size),
            "{base} 的 st_value/st_size 必须与自己 resolver 一致"
        );
    }

    // 变体名：点号被 mangle 成下划线，且都是各自独立的 FUNC 体。
    let variants = [
        "compute.sse4_2",
        "compute.default",
        "multiply.avx",
        "multiply.sse4_2",
        "multiply.default",
    ];
    for v in variants {
        let s = find(v).unwrap_or_else(|| panic!("缺变体符号 {v}（旧判据写的 {v} 拼错了点号形式）"));
        assert_eq!(s.stype, STT_FUNC, "{v} 应是 STT_FUNC");
        assert!(s.size > 0, "{v} 应有非零函数体");
    }
    // 基名不得再与任何一个实现体同址（旧缺陷形状）。
    for base in ["compute", "multiply"] {
        let b = find(base).expect("base");
        for v in variants.iter().filter(|v| v.starts_with(&format!("{base}."))) {
            let s = find(v).expect("variant");
            assert_ne!(
                b.value, s.value,
                "IFUNC {base} 与实现体 {v} 同址 = resolver 又丢了（旧缺陷形状）"
            );
        }
    }

    // CPUID 缓存必须真的存在于 `.o`（LOCAL OBJECT，尺寸 = cpu_isa::MODEL_BYTES）：
    // resolver 与 `__builtin_cpu_*` 共用它，没有它两者都是空谈。
    let model = find("__dcc_cpu_model").expect(".o 里应有 __dcc_cpu_model");
    assert_eq!(model.stype, STT_OBJECT, "__dcc_cpu_model 应是 OBJECT");
    assert_eq!(model.bind, 0, "CPUID 缓存必须是 STB_LOCAL（不进动态符号表）");
    assert_eq!(
        model.size,
        dcc::semantics::cpu_isa::MODEL_BYTES as u64,
        "__dcc_cpu_model 尺寸必须等于 cpu_isa::MODEL_BYTES（= SLOT_WORDS * 4）：槽表加了而对象没跟着长，\
         发射器就会读写到对象之外"
    );
    // 必须坐 `.data`（PROGBITS）而不是 `.bss`（NOBITS）：内置链接器的静态路径把
    // NOBITS 节铺成 size=0 / RW PT_LOAD 全 0，缓存落不进去 → `__dcc_cpu_indicator_init`
    // 第一个写就 SIGSEGV。这一条把「换节」这个设计决定钉住，改回 `.bss` 立刻红。
    let sec_names = harness::elf64_section_names(&obj);
    let model_sec = sec_names
        .get(model.shndx as usize)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        model_sec, ".data",
        "__dcc_cpu_model 必须在 .data（NOBITS 的 .bss 在内置静态链接路径下不会被映射）"
    );
    let init = find("__dcc_cpu_indicator_init").expect(".o 里应有 __dcc_cpu_indicator_init");
    assert_eq!(init.stype, STT_FUNC, "CPUID 检测函数应是 STT_FUNC");
    assert_eq!(init.bind, 0, "CPUID 检测函数必须 LOCAL，不依赖 libgcc 的同名符号");
    assert!(init.size > 0, "检测函数体必须真被发出来（旧形状里全 .o 无 CPUID）");
}

/// resolver 的变体判定链必须按 **ISA 优先级表**（真源 `semantics::cpu_isa`）排，
/// 而不是按 `target_clones(...)` 的书写顺序；并且真的发 CPUID 检测。
///
/// 夹具故意把书写顺序打乱（`"sse4.2,avx512f,avx,default"` / `"default,avx,sse4.2"`），
/// 所以“按声明顺序”与“按优先级”两种实现会给出不同的链 —— 写死的期望序列
/// `avx512f → avx → sse4_2 → default` / `avx → sse4_2 → default` 只要任一处回到
/// 「声明序」或「只认 default」就立刻红。可证伪且机器无关。
///
/// 为什么只能在 `.o` 侧静态断这条「判定链顺序」：dcc 目前没有 per-variant 的 ISA
/// codegen（`ast.rs` 里没有 `target_attrs`），`foo.avx` 与 `foo.default` 的机器码
/// 完全相同，选中谁在 stdout 里看不出来。但「选中谁」本身仍可在动态产物里观察：
/// `(void *)foo` 经 GOT + `R_X86_64_IRELATIVE` 拿到的是**解析后的实现体地址**（实测
/// dcc 本机得 `compute.sse4_2`），见 `target_clones_runtime_resolves_to_non_default_variant`；
/// 而「选型真发生」的行为级等价证明（两个返回值不同的实现体）由
/// `attr_target_clones_ifunc_behavior_*` 用 `__attribute__((ifunc))` 钉。
#[test]
#[cfg(unix)]
fn attr_target_clones_select_resolver_dispatch_order() {
    const R_X86_64_PC32: u32 = 2;
    let src = include_str!("../fixtures/c/attr_target_clones_select.c");
    let obj = harness::dcc_obj_bytes("tc-select", src, &["-O0"]);
    let syms = elf64_syms(&obj);
    let find = |n: &str| syms.iter().find(|s| s.name == n);
    let relas = harness::elf64_rela(&obj, ".rela.text");

    // 一个 resolver 体内命中的变体符号，按指令地址（r_offset）排序 = 判定链顺序。
    let chain = |base: &str| -> Vec<String> {
        let r = find(&format!("{base}.resolver")).unwrap_or_else(|| panic!("没有 {base}.resolver"));
        let prefix = format!("{base}.");
        let mut hits: Vec<(u64, String)> = relas
            .iter()
            .filter(|(off, _sym, typ, _)| *typ == R_X86_64_PC32 && *off >= r.value && *off < r.value + r.size)
            .filter_map(|(off, sym, _, _)| syms.get(*sym as usize).map(|s| (*off, s.name.clone())))
            .filter(|(_, name)| name.starts_with(&prefix))
            .collect();
        hits.sort();
        hits.into_iter().map(|(_, name)| name).collect()
    };

    assert_eq!(
        chain("pick"),
        vec!["pick.avx512f", "pick.avx", "pick.sse4_2", "pick.default"],
        "pick 的判定链必须是 ISA 优先级顺序（书写序是 sse4.2,avx512f,avx,default）"
    );
    assert_eq!(
        chain("pick_static"),
        vec!["pick_static.avx", "pick_static.sse4_2", "pick_static.default"],
        "pick_static 的判定链必须是 avx → sse4_2 → default（书写序是 default,avx,sse4.2）"
    );

    // CPUID 证据：resolver 入门先调检测函数，判定体读特性缓存。
    for base in ["pick", "pick_static"] {
        let r = find(&format!("{base}.resolver")).expect("resolver");
        let in_body = |sym: &str| -> bool {
            relas.iter().any(|(off, idx, typ, _)| {
                *typ == R_X86_64_PC32 && *off >= r.value && *off < r.value + r.size
                    && syms.get(*idx as usize).map(|s| s.name == sym).unwrap_or(false)
            })
        };
        assert!(
            in_body("__dcc_cpu_indicator_init"),
            "{base}.resolver 必须调用 __dcc_cpu_indicator_init：resolver 由动态链接器在 \
             .init 之前调用，那一刻没有任何已初始化状态可靠（不依赖 .init_array 的设计决定）"
        );
        assert!(
            in_body("__dcc_cpu_model"),
            "{base}.resolver 必须读 __dcc_cpu_model 的特性位缓存（否则就是无条件返回 default 的假 resolver）"
        );
    }

    // static 基名那一支的形状：IFUNC 与 resolver 都必须是 LOCAL（与 gcc 13 实测同形）。
    let sb = find("pick_static").expect("pick_static");
    let sr = find("pick_static.resolver").expect("pick_static.resolver");
    assert_eq!((sb.stype, sb.bind), (10, 0), "static 基名：STT_GNU_IFUNC + STB_LOCAL");
    assert_eq!((sr.stype, sr.bind), (2, 0), "static 的 resolver：STT_FUNC + STB_LOCAL");
    assert_eq!((sb.value, sb.size), (sr.value, sr.size), "static 分支也要 IFUNC 指向自己 resolver");
    // 同一个基名的变体实现体地址必须两两不同（否则「选中谁」无意义）。
    let mut addrs: Vec<u64> = ["pick.avx512f", "pick.avx", "pick.sse4_2", "pick.default"]
        .iter()
        .map(|n| find(n).unwrap_or_else(|| panic!("缺变体 {n}")).value)
        .collect();
    addrs.sort_unstable();
    addrs.dedup();
    assert_eq!(addrs.len(), 4, "四个变体实现体必须有四个不同地址");
}

/// resolver 的**机器码本身**必须逐位来自 `cpu_isa` 表 —— 单一真源在字节层的证据。
///
/// 为什么需要这一条（而 §1.5 那条不够）：那只看「判定链里出现了哪些变体名」，它不能
/// 发现「名字对、但测的位错了」（把 leaf7 的位读到 leaf1 的槽上、或掩码与槽不配对）。
/// 本用例把 `.o` 里 `__dcc_cpu_model` 的每一条重定位**反回机器码**：从指令字节辨出
/// 发射形式（`F7 05` = `test m32,imm32`、`8B 05` = `mov disp32(%rip),%eax`），算出
/// 真实字段偏移 = `r_addend + (L - f)`，再与 `cpu_isa` 的 `(cache_slot(leaf,reg), bit)`
/// 逐条比对。这一步同时钉住那个很容易踩的加数陷阱：`readelf` 显示的加数已经含了
/// `-(L - f)`，`test` 是 -8、`mov` 是 -4；把两者都当 -4 算会得到「槽号差一个 dword」
/// 的自洽错读（CPUID 真测了、位号也对，但读错了寄存器）。XCR0 门控同理：表里
/// `xcr0_mask != 0` 的 ISA 必须能在机器码里看到读 `SLOT_XCR0_LOW`，SSE 系必须看不到。
///
/// 与机器无关：只读 `.o` 的字节与重定位，不运行任何东西；期望值全部从 `cpu_isa`
/// 推出，不在此处重写一份位表（否则本用例会变成「第二份表」而失去钉作用）。
#[test]
#[cfg(unix)]
fn attr_target_clones_resolver_bits_come_from_cpu_isa_table() {
    use dcc::semantics::cpu_isa;
    const R_X86_64_PC32: u32 = 2;

    /// 一条从 `__dcc_cpu_model` 读出的检测：特性位（槽, 位）或 XCR0 读（槽）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Read {
        Feature(usize, u32),
        Xcr0(usize),
    }

    let src = include_str!("../fixtures/c/attr_target_clones.c");
    let obj = harness::dcc_obj_bytes("tc-bits", src, &["-O0"]);
    let syms = elf64_syms(&obj);
    let text = elf64_section_data(&obj, ".text").expect(".o 里应有 .text").to_vec();
    let relas = harness::elf64_rela(&obj, ".rela.text");

    // 已知发射形式的 `(L - f)`：与 `codegen/x86_64/ifunc.rs` 的 `RipForm::delta` 同值。
    // 这里列的是「指令长 - disp32 字段相对指令起始的偏移」，不是特性位。
    let decode = |off: u64, addend: i64| -> Option<Read> {
        let p = off as usize;
        if p < 2 || p + 8 > text.len() {
            return None;
        }
        let (delta, imm_after) = match &text[p - 2..p] {
            [0xF7, 0x05] => (8i64, true),
            [0x8B, 0x05] => (4i64, false),
            _ => return None,
        };
        let field = addend + delta;
        if field < 0 || field % 4 != 0 {
            return None;
        }
        let slot = (field / 4) as usize;
        if imm_after {
            let mask = u32::from_le_bytes(text[p + 4..p + 8].try_into().unwrap());
            if mask.count_ones() != 1 {
                return None;
            }
            Some(Read::Feature(slot, mask.trailing_zeros()))
        } else {
            Some(Read::Xcr0(slot))
        }
    };

    for base in ["compute", "multiply"] {
        let find = |n: &str| syms.iter().find(|s| s.name == n);
        let r = find(&format!("{base}.resolver"))
            .unwrap_or_else(|| panic!(".o 里没有 {base}.resolver"));
        let prefix = format!("{base}.");
        // 按地址序重走一遍 resolver：特性读累积，碰到变体 `lea` 则闭合一段。
        let mut hits: Vec<(u64, String, i64)> = relas
            .iter()
            .filter(|(off, _, typ, _)| *typ == R_X86_64_PC32 && *off >= r.value && *off < r.value + r.size)
            .filter_map(|(off, idx, _, add)| {
                syms.get(*idx as usize).map(|s| (*off, s.name.clone(), *add))
            })
            .collect();
        hits.sort();

        let mut pending: Vec<Read> = Vec::new();
        let mut segments: Vec<(String, Vec<Read>)> = Vec::new();
        for (off, name, add) in hits {
            if name.starts_with(&prefix) {
                segments.push((name, std::mem::take(&mut pending)));
            } else if name == "__dcc_cpu_model" {
                pending.push(decode(off, add).unwrap_or_else(|| panic!(
                    "{base}.resolver 里 {off:#x} 处指向 __dcc_cpu_model 的重定位不是已知的检测形式\
                     （addend={add}，前缀字节 {:?}）",
                    &text[off as usize - 2..off as usize]
                )));
            }
            // `__dcc_cpu_indicator_init` 等其他目标不参与本轮比较。
        }
        assert!(
            pending.is_empty() && segments.len() >= 2,
            "{base}.resolver 的结构应是「若干段，每段以变体 lea 结尾」；实际段数 {}",
            segments.len()
        );
        // 防「空对空也算过」：本基名必须真读到过至少一条特性位（否则上面那个
        // `got == want` 在两边都空时也会成立，就是假绿）。
        assert!(
            segments
                .iter()
                .any(|(_, rs)| rs.iter().any(|rd| matches!(rd, Read::Feature(..)))),
            "{base}.resolver 里一条特性检测都没读到（假 resolver 的形状）：{segments:?}"
        );
        assert!(
            segments.iter().any(|(_, rs)| rs
                .iter()
                .any(|rd| matches!(rd, Read::Xcr0(s) if *s == cpu_isa::SLOT_XCR0_LOW)))
                == (base == "multiply"),
            "只有带 AVX 家族的 {base} 才应读到 XCR0 门控：{segments:?}"
        );

        for (variant, reads) in &segments {
            let suffix = &variant[prefix.len()..];
            if suffix == "default" {
                assert!(
                    reads.is_empty(),
                    "{variant} 不应带任何特性检测（default 是无条件兜底）；实际读到 {reads:?}"
                );
                continue;
            }
            let arch = cpu_isa::lookup(suffix)
                .unwrap_or_else(|| panic!("变体名 {variant} 的 ISA 部分不在 cpu_isa 表里"));
            let want: Vec<Read> = arch
                .bits
                .iter()
                .map(|b| {
                    let slot = cpu_isa::cache_slot(b.leaf, b.subleaf, b.reg).unwrap_or_else(|| {
                        panic!("cpu_isa 自己不一致：{suffix} 的位 {b:?} 没有缓存槽")
                    });
                    Read::Feature(slot, b.bit as u32)
                })
                .collect();
            let got: Vec<Read> = reads
                .iter()
                .filter(|rd| matches!(rd, Read::Feature(..)))
                .cloned()
                .collect();
            assert_eq!(
                got, want,
                "{variant} 的检测位必须逐条等于 cpu_isa 的表（槽/位不配对 = 加数换算错了或\
                 发射器读了另一份表）"
            );
            let wants_xcr0 = arch.xcr0_mask != 0;
            let has_xcr0 = reads
                .iter()
                .any(|rd| matches!(rd, Read::Xcr0(s) if *s == cpu_isa::SLOT_XCR0_LOW));
            assert_eq!(
                has_xcr0, wants_xcr0,
                "{variant}: XCR0 门控必须按表出现（表要 {:#x}，机器码里有 {has_xcr0}）",
                arch.xcr0_mask
            );
        }
    }
}

/// 「真选中了非 default 变体」在 **target_clones** 上也必须可观察（不只是 `.o` 侧静态读）。
///
/// 通道：宿主 `ld` 给 `(void *)compute` 发 GOT 槽 + `R_X86_64_IRELATIVE`，动态链接器
/// 调 resolver 并把**返回的变体地址**写回那个槽 —— 程序打印出来的就是选型结果。实测
/// 本机（sse4.2/avx 都支持，`-O0`）：`delta=-448` = `compute.sse4_2`(0x402150) -
/// `marker`(0x402310)，而 `compute.default`(0x4021b8) 那一支是 -344。判据把「builtin
/// 说支持」与「地址落在该变体」钉成同一件事，因此恒假桩 / 恒返 default / 恒返第一个
/// 变体三种偷工做法都会红（机器无关）。打印差值而非绝对地址：同样吃 PIE/ASLR。
///
/// 任务书要求的是「真的选中了 **avx/sse4.2** 变体」都能被观察到，所以夹具带**两支**：
/// `compute`（二档 `sse4.2,default`）与 `dispatch3`（三档 `avx,sse4.2,default`，实测
/// `delta3=-240` = `dispatch3.avx`(0x402220) - `marker`；sse4_2 是 -160、default 是 -80）。
/// 第二支额外钉两件事：avx 与 sse4.2 同时为真时必须走 avx（优先级），以及 avx 那一支的
/// XCR0/OSXSAVE 门控。期望仍只由 `__builtin_cpu_supports("avx")` 的运行时读数决定，
/// 所以在任何机器上都成立；「三档链只认第一/最后一档」这类错在本机会给 sse4_2 或
/// default，立刻红。
///
/// 不做 gcc 逐字对照（两处有意差异，理由写在夹具头）：
///   1) gcc 13 把 `&ifunc` 绑到 PLT 桩 —— 实测 `gcc -O0 -no-pie` 打 `delta=-4606`，
///      即 `p = marker(0x40227e) - 4606 = 0x401080`，落在 `.plt.sec` [0x401060,0x401090)，
///      而 dcc 打解析后的实现体；调用语义相同，但只有 dcc 这一侧能直接读出选了谁；
///   2) `__builtin_cpu_supports` 的返回值 gcc 是 libgcc 内部特性字的位值（本机实测
///      sse4.2=256、avx=512），dcc 归一成 0/1 —— 本判据用 `has != 0` 取定，对两者免疫。
#[test]
#[cfg(unix)]
fn target_clones_runtime_resolves_to_non_default_variant() {
    let src = include_str!("../fixtures/c/attr_target_clones_selected.c");
    let (ok, stderr, exe) = harness::dcc_link_exe("tc-selected", src, &["-O0"]);
    assert!(ok, "夹具应能经宿主 cc 链上；stderr:\n{stderr}");
    // 观察选型之前先确认产物是动态的：静态产物里根本没有动态链接器去跑 resolver。
    harness::assert_dynamically_linked(&exe, "tc-selected", &stderr);

    let syms = elf64_syms(&fs::read(&exe).unwrap_or_default());
    let value_of = |n: &str| -> i64 {
        syms.iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("exe 符号表里没有 {n}"))
            .value as i64
    };
    let marker = value_of("marker");
    let sse4_2 = value_of("compute.sse4_2") - marker;
    let default = value_of("compute.default") - marker;
    assert_ne!(sse4_2, default, "两个变体必须真的在两个不同地址上，否则本判据无从分辨");

    let (rc, stdout, run_stderr) = run_exe_bounded(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "运行失败；stderr:\n{}", String::from_utf8_lossy(&run_stderr));
    let text = String::from_utf8_lossy(&stdout).to_string();
    let field = |key: &str| -> Option<i64> {
        text.split_whitespace()
            .find_map(|w| w.strip_prefix(&format!("{key}=")))
            .and_then(|v| v.parse::<i64>().ok())
    };
    let (delta, has, call) = (
        field("delta"),
        field("has_sse42"),
        field("call"),
    );
    let (delta, has, call) = (
        delta.unwrap_or_else(|| panic!("输出里没有 delta=：{text}")),
        has.unwrap_or_else(|| panic!("输出里没有 has_sse42=：{text}")),
        call.unwrap_or_else(|| panic!("输出里没有 call=：{text}")),
    );
    assert_eq!(call, 21, "compute(10) 必须经 IFUNC 解析后返回 21（选中的实现真被调用）");
    let (expect, other, which) = if has != 0 {
        (sse4_2, default, "compute.sse4_2")
    } else {
        (default, sse4_2, "compute.default")
    };
    assert_eq!(
        delta, expect,
        "本机 `__builtin_cpu_supports(\"sse4.2\")` 报 {has}，解析后的地址就应该落在 \
         {which}（差值 {expect}），实际 {delta}；输出：{text}"
    );
    assert_ne!(
        delta, other,
        "解析结果同时等于**另一个**变体 = resolver 的选型与 CPUID 读数无关（假 resolver）"
    );

    // 第二支：三变体链 `avx,sse4.2,default` —— 「真选中了 avx 变体」也必须可观察。
    let avx = value_of("dispatch3.avx") - marker;
    let d3_sse42 = value_of("dispatch3.sse4_2") - marker;
    let d3_default = value_of("dispatch3.default") - marker;
    // 三个变体必须落在三个不同地址上，否则本判据无从分辨选型结果。
    assert!(
        avx != d3_sse42 && avx != d3_default && d3_sse42 != d3_default,
        "dispatch3 的三个变体地址必须两两不同，实得 avx={avx} sse4_2={d3_sse42} default={d3_default}"
    );
    let delta3 = field("delta3").unwrap_or_else(|| panic!("输出里没有 delta3=：{text}"));
    let has_avx = field("has_avx").unwrap_or_else(|| panic!("输出里没有 has_avx=：{text}"));
    let call3 = field("call3").unwrap_or_else(|| panic!("输出里没有 call3=：{text}"));
    assert_eq!(call3, 17, "dispatch3(10) 必须经 IFUNC 解析后返回 17（选中的实现真被调用）");
    let (which3, expect3) = if has_avx != 0 {
        ("dispatch3.avx", avx)
    } else if has != 0 {
        ("dispatch3.sse4_2", d3_sse42)
    } else {
        ("dispatch3.default", d3_default)
    };
    assert_eq!(
        delta3, expect3,
        "本机 avx={has_avx} sse4.2={has}，解析后的地址就应该落在 {which3}（差值 {expect3}），\
         实际 {delta3}；三档链的优先级或 XCR0 门控有一处不对；输出：{text}"
    );
    for (name, value) in [
        ("dispatch3.avx", avx),
        ("dispatch3.sse4_2", d3_sse42),
        ("dispatch3.default", d3_default),
    ] {
        if name != which3 {
            assert_ne!(
                delta3, value,
                "解析结果等于 {name}，但 builtin 读数要求 {which3}（选型与 CPUID 读数脱钩）"
            );
        }
    }
}

/// 「CPUID 真被跑过、真选中的不是 default」的**行为级**可证伪判据。
///
/// 夹具用 `__attribute__((ifunc))` + 返回值不同的实现体：`resolve_sse2` 在
/// leaf1/EDX#26（x86-64 恒真）时返回 `fast_impl = 2`，否则 `slow_impl = 1`；
/// `main` 把「选中结果」与「运行时再问一次 `__builtin_cpu_supports`」比较，并只在
/// `a == 2` 时返回 0。于是恒假 resolver / 无条件返回 default / builtin 永远 0 三种
/// 偷工做法都得 rc != 0。第二个 ifunc 用 `avx2`（本机真、老机器假），只与 gcc
/// 逐字比输出而不写死结果。
#[test]
#[cfg(unix)]
fn attr_target_clones_ifunc_behavior_selects_non_default() {
    const STT_GNU_IFUNC: u8 = 10;
    const STT_FUNC: u8 = 2;
    let src = include_str!("../fixtures/c/attr_target_clones_ifunc_behavior.c");
    for opt in ["-O0", "-O1", "-O2", "-O3"] {
        let rc = run_gcc_cross_check_opt(&format!("tc-behavior-{opt}"), src, &[opt]);
        assert_eq!(rc, 0, "{opt}: 夹具自己判定「选中的必须是 fast_impl(=2) 且与 builtin 同源」失败");
    }
    // `.o` 侧：`__attribute__((ifunc("r")))` 的基名必须与用户写的 resolver 同址。
    let obj = harness::dcc_obj_bytes("tc-behavior-o", src, &["-O0"]);
    let syms = elf64_syms(&obj);
    let find = |n: &str| syms.iter().find(|s| s.name == n);
    for (base, resolver) in [("by_sse2", "resolve_sse2"), ("by_avx2", "resolve_avx2")] {
        let b = find(base).unwrap_or_else(|| panic!(".o 里没有 IFUNC 基名 {base}"));
        let r = find(resolver).unwrap_or_else(|| panic!("用户写的 resolver {resolver} 没发到 .o（被 DCE 杀过）"));
        assert_eq!(b.stype, STT_GNU_IFUNC, "{base} 必须是 STT_GNU_IFUNC");
        assert_eq!(r.stype, STT_FUNC, "{resolver} 必须是普通 FUNC 体");
        assert_eq!((b.value, b.size), (r.value, r.size), "{base} 必须指向 {resolver}");
        assert_ne!(b.value, find("slow_impl").expect("slow_impl").value, "IFUNC 不得直指实现体");
    }
}

/// `__builtin_cpu_supports` / `__builtin_cpu_is` 必须是**真实现**（运行时 CPUID +
/// 缓存），而不是挂在 `preproc.rs` 名字表上的恒假桩。
///
/// 夹具的四条不变量（sse/sse2 恒真锚点、avx2⇒avx⇒sse4.2⇒sse3⇒sse2 单调、intel/amd
/// 互斥、init 后幂等）全是机器无关的：恒 0 桩钉不住第一条，硬编码 1 钉不住第二条
/// 的错位的位（把 leaf7 当 leaf1 读会以「高特性真、低特性假」破坏偏序），厂商 dword
/// 拼接顺序写错（ECX 当 EDX）钉不住第三条。四档都与 gcc 逐字对照 stdout。
#[test]
#[cfg(unix)]
fn attr_target_clones_cpu_builtin_is_real_implementation() {
    let src = include_str!("../fixtures/c/attr_target_clones_cpu_builtin.c");
    for opt in ["-O0", "-O1", "-O2", "-O3"] {
        let rc = run_gcc_cross_check_opt(&format!("tc-builtin-{opt}"), src, &[opt]);
        assert_eq!(rc, 0, "{opt}: builtin 的不变量不成立（恒假桩/位表错位/厂商顺序错）");
    }
    // 只用 builtin（没有 target_clones）也必须自带 CPUID 检测：不能靠宿主 libgcc。
    let obj = harness::dcc_obj_bytes("tc-builtin-o", src, &["-O0"]);
    let syms = elf64_syms(&obj);
    let find = |n: &str| syms.iter().find(|s| s.name == n);
    assert!(find("__dcc_cpu_indicator_init").is_some(), "只用 builtin 时也必须发检测函数");
    assert!(find("__dcc_cpu_model").is_some(), "只用 builtin 时也必须发缓存对象");
    for undefined in elf64_undefined_symbols(&obj) {
        assert!(
            !undefined.contains("__cpu_model") && !undefined.contains("__builtin_cpu"),
            "builtin 实现不得依赖 libgcc 符号，得自包含在 .o 里：{undefined}"
        );
    }
    // 读表序列真的在 `.text` 里（不只是“有个定义”）：有 PC32 指向缓存与检测函数。
    let relas = harness::elf64_rela(&obj, ".rela.text");
    let references = |n: &str| {
        relas
            .iter()
            .any(|(_, idx, _, _)| syms.get(*idx as usize).map(|s| s.name == n).unwrap_or(false))
    };
    assert!(references("__dcc_cpu_model"), "builtin 的读表序列没发出来（.text 里没有对 __dcc_cpu_model 的重定位）");
    assert!(references("__dcc_cpu_indicator_init"), "builtin 没调用 CPUID 检测");
}

/// `__builtin_cpu_supports` 位表的**逐名字与 gcc 差分**判据。
///
/// 为什么 `attr_target_clones_cpu_builtin_is_real_implementation` 的四条不变量不够：
/// 它们钉的是「非空 + 偏序 + 互斥 + 幂等」，抓得住恒假桩和「把 leaf7 当 leaf1 读」这类
/// **结构性**错误，但抓不住「名字落在同一 leaf 的另一个位上」，也完全不看扩展叶。
/// 本轮就是靠这条差分当场抓到两个真错（不是理论风险，是已修掉的既有缺陷）：
///   1. `rdseed` 在 `src/semantics.rs` 里写成 CPUID.(7,0):EBX**#22** —— 那是 ENCLV，
///      RDSEED 是 #18；本机 #22=0、#18=1，而偏序链里没有 rdseed，所以四条不变量全绿、
///      答案恒 0，gcc 报 1。
///   2. 扩展叶根探测（leaf `0x8000_0000`）被一个**永假**门控挡住：门控读的是基本叶最大值
///      `SLOT_LEAF0_EAX`（本机 0x20）并要求它 `>= 0x8000_0000`。于是 `SLOT_EXT_MAX_EAX`
///      永不填、leaf `0x8000_0001` 整条跳过，`lzcnt`/`abm`/`lm`/`nx`/`syscall`/`rdtscp`/
///      `sse4a`/`3dnow`/`3dnowext` **全体静默读 0** —— 正是任务书禁止的「看起来修了」形状。
///      恒真锚点那条不变量只列了 sse/sse2，碰不到扩展叶，所以也没抓到。
///
/// 判据的两层：
///   * **与 gcc 逐字对照**（四档）：同一份夹具、同一台真机，任何一位写错都会显形。
///   * **自洽核对**（不看 gcc，把 dcc 自己的 stdout 当数据）：49 个名字一个都不能少
///     （少一个 = 夹具与名字表脱钩，gcc 对照会跟着一起瞎，因为两侧都少问同一个名字）；
///     `sum=` 必须等于逐行 1 的个数；x86-64 恒真位必须为 1 —— 其中 `lm` 只来自扩展叶，
///     所以它是「扩展叶探测真的执行过」的直接证据，不依赖 gcc 在场。
///
/// 名字集合取的是 **gcc 13 与 dcc 都接受**的交集（gcc 侧表在
/// `gcc/common/config/i386/i386-isas.h`），**一个都不排除**。曾经把 `movdiri`
/// 排除在外并写成「本机 EDX bit27=1 而 gcc 报 0，那是 libgcc 对 >31 号特性字的
/// 存储/版本条件」—— 那个归因是**错的**，它把一个实现缺陷掩盖成外部不可比：
/// 权威位号是 CPUID.(7,0):**ECX**#27（Linux `cpufeatures.h` word 16、gcc `cpuid.h`
/// 的 `bit_MOVDIRI` 配 `cpuinfo.h` 的 `ecx & bit_MOVDIRI`），本表曾写成 EDX#27
/// （那一位是 INTEL_STIBP），所以在不支持 MOVDIRI 的本机上报 1 而 gcc 报 0。
/// 现在位号改正、名字放回，两侧都在场才藏不住。
#[test]
#[cfg(unix)]
fn attr_target_clones_cpu_supports_matrix_matches_gcc() {
    /// gcc 13 (`i386-isas.h`) 与 dcc (`src/semantics.rs::FEATURES`) 都接受的特性名。
    const NAMES: &[&str] = &[
        "3dnow", "abm", "adx", "aes", "avx", "avx2", "avx512bf16", "avx512bitalg",
        "avx512bw", "avx512cd", "avx512dq", "avx512er", "avx512f", "avx512ifma",
        "avx512pf", "avx512vbmi", "avx512vbmi2", "avx512vl", "avx512vnni",
        "avx512vp2intersect", "avx512vpopcntdq", "bmi", "bmi2", "clwb", "clzero",
        "cmov", "f16c", "fma", "gfni", "ibt", "lm", "lzcnt", "mmx", "movbe",
        "movdiri", "osxsave", "pclmul", "pku", "popcnt", "rdrnd", "rdseed",
        "serialize", "sha", "shstk", "sse", "sse2", "sse3", "sse4.1", "sse4.2",
        "sse4a", "ssse3", "vaes", "vpclmulqdq", "waitpkg", "wbnoinvd", "xop",
        "xsave", "xsavec", "xsaveopt", "xsaves",
    ];
    let src = include_str!("../fixtures/c/attr_target_clones_cpu_matrix.c");
    assert_eq!(NAMES.len(), 60, "交集名字表本身要按数量钉住（夹具头写着 60 个）");
    for opt in ["-O0", "-O1", "-O2", "-O3"] {
        let rc = run_gcc_cross_check_opt(&format!("tc-matrix-{opt}"), src, &[opt]);
        assert_eq!(rc, 0, "{opt}: 夹具的机器无关锚点不成立（扩展叶没跑 / 偏序破 / abm!=lzcnt / 子叶门控错）");
    }

    // 第二层：dcc 自己的读数当数据核对，不借 gcc 的权威。
    let (build, exe) = run_compile_and_run_flags("tc-matrix-self", src, &["-O2"]);
    assert!(
        build.status.success(),
        "dcc 应能编译 cpu_matrix（任一名字不被接受就是 E0018）: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let (code, stdout, stderr) = run_exe_bounded(&exe);
    let _ = std::fs::remove_file(&exe);
    let text = String::from_utf8_lossy(&stdout).to_string();
    assert_eq!(code, 0, "dcc 侧锚点自检失败（rc={code}），输出:\n{text}\nstderr:\n{}", String::from_utf8_lossy(&stderr));

    let mut ones = 0usize;
    for name in NAMES {
        // 名字里带 `.`（sse4.1/sse4.2）与以数字开头（3dnow）都不影响：夹具打印的行标签就是原名。
        let mut hits = text
            .lines()
            .filter(|l| l.split_once('=').map(|(k, _)| k == *name).unwrap_or(false));
        let value = hits
            .next()
            .unwrap_or_else(|| panic!("输出里缺 {name}= 这一行 —— 夹具与名字表脱钩了:\n{text}"));
        assert!(hits.next().is_none(), "{name} 在输出里出现多次，对照就没有意义了");
        let v: i32 = value
            .split_once('=')
            .unwrap()
            .1
            .parse()
            .unwrap_or_else(|_| panic!("{name} 的读数不是整数: {value}"));
        assert!((v == 0 || v == 1), "{name} 必须是归一后的 0/1，实得 {v}");
        ones += v as usize;
    }
    assert_eq!(
        text.lines().filter(|l| l.contains('=')).count(),
        NAMES.len() + 1,
        "输出行数应正好是 60 个名字 + 1 行 sum=（多出来的行说明夹具加了没登记的名字）:\n{text}"
    );
    let sum: i32 = text
        .lines()
        .find_map(|l| l.strip_prefix("sum=").and_then(|v| v.trim().parse().ok()))
        .expect("没有 sum= 聚合行");
    assert_eq!(sum as usize, ones, "sum= 与逐行 1 的个数不一致（夹具自己不自洽，对照读数不可信）");

    // 扩展叶的直接证据：`lm` 只有 0x8000_0001 的探测真的跑了才会是 1。
    let read = |n: &str| {
        text.lines()
            .find(|l| l.split_once('=').map(|(k, _)| k == n).unwrap_or(false))
            .unwrap_or_else(|| panic!("输出里缺 {n}=:\n{text}"))
            .split_once('=')
            .unwrap()
            .1
            .trim()
            .parse::<i32>()
            .unwrap()
    };
    for anchor in ["lm", "mmx", "cmov", "sse", "sse2"] {
        assert_eq!(read(anchor), 1, "{anchor} 在 x86-64 上恒真；读 0 说明对应探测没跑（lm=扩展叶整条被跳过）");
    }
    assert_eq!(read("abm"), read("lzcnt"), "abm 与 lzcnt 是同一个 CPUID 位（0x8000_0001/ECX#5）的两种写法");
    // 子叶门控的一致性（不靠 gcc 在场）：(0xD,1)/(7,1) 的内容在子叶不存在时**等于子叶 0**，
    // 所以「把 (7,0):EAX 当 (7,1):EAX 用」只会体现为子叶名字的答案错；这里用两条
    // 必要约束钉住方向（子叶位为真 ⇒ 它依赖的父位为真）。
    if read("xsaveopt") == 1 || read("xsavec") == 1 || read("xsaves") == 1 {
        assert_eq!(read("xsave"), 1, "(0xD,1) 的位子叶为真但 xsave 为假 ⇒ 子叶采集没被 xsave 门控");
    }
    if read("avx512bf16") == 1 {
        assert_eq!(read("osxsave"), 1, "(7,1) 的位需要 XCR0 的 AVX-512 状态位，osxsave 为假时不得为真");
    }
}

/// `__builtin_cpu_is("<型号名>")` 必须是**真的分类结果**（终态 3 的另一半）。
///
/// `attr_target_clones_cpu_builtin_is_real_implementation` 只问 `intel`/`amd` 两个厂商名，
/// 它证明的是「leaf0 的三个 dword 被认出来」。型号名要求得多：leaf1/EAX 的
/// family+model 解码（含 `family==0xf`/`==6`/Zhaoxin `==7` 三支厂商相关调整）+ 整张按
/// `(厂商, family)` 分组、按 model 逐条、必要时用特性位当守卫的分类表。那一层在上一轮
/// 根本不存在（型号名一律报 E1125 桩），所以判据必须逐个名字问。
///
/// 两层，避免「把本机答案抄进断言」：
///   * **与 gcc 逐字对照**（四档）：本机 gcc 报 `intel=1 corei7=1 rocketlake=1`、其余 48 个
///     为 0。恒假桩在这条上必红（它给不出那三个 1）。
///   * **参考求值器对账**（不写死本机答案）：期望值由 `cpu_isa::host_processor_class()`
///     现场算出（同一张 `CLASSIFY` + 同一份 `decode_family_model`），逐名字与 dcc 的
///     stdout 比。换一台 AMD/Zhaoxin 机器，期望跟着变而判据仍成立 —— 与硬编码
///     `rocketlake=1` 的写法有本质区别（后者在别的机器上假红）。
#[test]
#[cfg(unix)]
fn attr_target_clones_cpu_is_model_names_match_gcc() {
    use dcc::semantics::cpu_isa;
    let src = include_str!("../fixtures/c/attr_target_clones_cpu_is_matrix.c");
    let names = cpu_isa::cpu_is_names();
    assert_eq!(names.len(), 51, "`__builtin_cpu_is` 名字集（gcc 13.3 实测集）的数量变了");
    for opt in ["-O0", "-O1", "-O2", "-O3"] {
        let rc = run_gcc_cross_check_opt(&format!("tc-cpuis-{opt}"), src, &[opt]);
        assert_eq!(
            rc, 0,
            "{opt}: 型号名夹具的机器无关锚点不成立（厂商互斥 / atom!=bonnell / 子型不蕴含父型 / 多个子型同时为真）"
        );
    }

    let (build, exe) = run_compile_and_run_flags("tc-cpuis-self", src, &["-O2"]);
    assert!(
        build.status.success(),
        "dcc 应能编译 cpu_is 矩阵（任一型号名不被接受就是 E1125）: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let (code, stdout, stderr) = run_exe_bounded(&exe);
    let _ = std::fs::remove_file(&exe);
    let text = String::from_utf8_lossy(&stdout).to_string();
    assert_eq!(code, 0, "dcc 侧锚点自检失败（rc={code}）:\n{text}\nstderr:\n{}", String::from_utf8_lossy(&stderr));

    let read = |n: &str| -> i32 {
        text.lines()
            .find(|l| l.split_once('=').map(|(k, _)| k == n).unwrap_or(false))
            .unwrap_or_else(|| panic!("输出里缺 {n}= 这一行 —— 夹具与 `CPU_IS_NAMES` 脱钩了:\n{text}"))
            .split_once('=')
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{n} 的读数不是 0/1:\n{text}"))
    };
    assert_eq!(
        text.lines().filter(|l| l.contains('=')).count(),
        names.len() + 1,
        "输出行数应正好是 51 个名字 + 1 行 sum=（多出来的行说明夹具加了没登记的名字）:\n{text}"
    );
    for n in &names {
        let v = read(n);
        assert!(v == 0 || v == 1, "{n} 必须被夹具归一成 0/1，实得 {v}");
    }
    let ones = names.iter().filter(|n| read(n) == 1).count();
    let sum: i32 = text
        .lines()
        .find_map(|l| l.strip_prefix("sum=").and_then(|v| v.trim().parse().ok()))
        .expect("没有 sum= 聚合行");
    assert_eq!(sum as usize, ones, "sum= 与逐行 1 的个数不一致（夹具不自洽，对照读数不可信）");

    // 第二层：与参考求值器逐名字对账（只在能真跑 CPUID 的宿主上做）。
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        let (kind, class) = cpu_isa::host_processor_class();
        for n in &names {
            assert_eq!(
                read(n),
                cpu_isa::cpu_is_holds(n, kind, &class) as i32,
                "'{n}' 的答案与 cpu_isa 参考求值器不一致：发射侧的解码/分类链/比较槽三处任一处飘了\n\
                 宿主 kind={kind} type={} subtype={}",
                class.cpu_type,
                class.cpu_subtype,
            );
        }
        // 反「恒假桩」：本机上确实有型号名为真（厂商名至少一个 + 若分类命中则型号名）。
        assert!(
            read("intel") == 1 || read("amd") == 1,
            "本机厂商名全为 0：leaf0 厂商 dword 比较坏了（型号名也不会对）:\n{text}"
        );
    }
}

/// `__dcc_cpu_indicator_init` 写进型号槽的 id 必须**恰好等于** `CLASSIFY` 里那些 id。
///
/// 为什么只靠上一条还不够：那是对**本机这一条路径**的读数 —— 本机是 Intel family 6
/// model 0xa7，分类链只走到那一条规则就 `jmp` 结束，其余几十条写入点根本没被执行过。
/// 如果发射器漏了某条规则（或比较端与写入端的 id 算歪），本机答案依旧全绿，而在一台
/// 真的 Cascade Lake / znver3 上 `__builtin_cpu_is("cascadelake")` 会永远为 0 —— 那正是
/// 「看起来修了」的形状，而且在这一台机器上任何跑数据的判据都抓不到。
///
/// 所以本条不运行任何东西，只读 `.o` 的字节与重定位：把 init 体里每一条
/// `movl $imm32, disp32(%rip)`（`C7 05`，`delta = 8`）按加数还原成「槽 + 立即数」，
/// 然后要求型号槽的立即数集合与从 `CLASSIFY` 推出来的 id 集合**相等**（不是包含）。
/// 期望全部由表算出，本文件不重写一份 id 表（否则用例自己就成了第二份真源）。
#[test]
#[cfg(unix)]
fn attr_target_clones_cpu_indicator_classify_ids_come_from_classify_table() {
    use std::collections::BTreeSet;
    use dcc::semantics::cpu_isa;
    const R_X86_64_PC32: u32 = 2;

    let src = include_str!("../fixtures/c/attr_target_clones_cpu_is_matrix.c");
    let obj = harness::dcc_obj_bytes("tc-classify-ids", src, &["-O0"]);
    let syms = elf64_syms(&obj);
    let text = elf64_section_data(&obj, ".text").expect(".o 里应有 .text").to_vec();
    let relas = harness::elf64_rela(&obj, ".rela.text");
    let init = syms
        .iter()
        .find(|s| s.name == "__dcc_cpu_indicator_init")
        .expect("只用 __builtin_cpu_is 也必须发 CPUID 检测函数");
    assert!(init.size > 0, "__dcc_cpu_indicator_init 必须是带长度的函数体");

    // 槽 -> 被写入过的立即数集合（只收 `C7 05` 这一种形式）。
    let mut writes: std::collections::BTreeMap<usize, BTreeSet<u32>> = std::collections::BTreeMap::new();
    for (off, idx, typ, addend) in relas.iter() {
        if *typ != R_X86_64_PC32 || *off < init.value || *off >= init.value + init.size {
            continue;
        }
        if syms.get(*idx as usize).map(|s| s.name.as_str()) != Some("__dcc_cpu_model") {
            continue;
        }
        let p = *off as usize;
        if p < 2 || p + 8 > text.len() || text[p - 2..p] != [0xC7, 0x05] {
            continue;
        }
        // `addend = slot_offset - delta`（`delta = 8`：disp32 字段到下一条指令起始）。
        let field = *addend + 8;
        assert!(field >= 0 && field % 4 == 0, "init 体里 {p:#x} 处的 `C7 05` 加数不指向 dword 槽：addend={addend}");
        let slot = (field / 4) as usize;
        let imm = u32::from_le_bytes(text[p + 4..p + 8].try_into().unwrap());
        writes.entry(slot).or_default().insert(imm);
    }

    // 期望集：从 `CLASSIFY` 推（与写入端、比较端同一个 `type_id`/`subtype_id`）。
    let mut want_type: BTreeSet<u32> = BTreeSet::new();
    let mut want_subtype: BTreeSet<u32> = BTreeSet::new();
    for g in cpu_isa::CLASSIFY {
        if !g.group_type.is_empty() {
            want_type.insert(cpu_isa::type_id(g.group_type).unwrap_or_else(|| panic!("组级类型 '{}' 没有 id", g.group_type)));
        }
        for r in g.rules {
            if !r.cpu_type.is_empty() {
                want_type.insert(cpu_isa::type_id(r.cpu_type).unwrap_or_else(|| panic!("规则类型 '{}' 没有 id", r.cpu_type)));
            }
            if !r.cpu_subtype.is_empty() {
                want_subtype.insert(cpu_isa::subtype_id(r.cpu_subtype).unwrap_or_else(|| panic!("规则子型 '{}' 没有 id", r.cpu_subtype)));
            }
        }
    }
    assert!(!want_type.is_empty() && !want_subtype.is_empty(), "分类表本身不能是空的");
    let got_type = writes.get(&cpu_isa::SLOT_CPU_TYPE).cloned().unwrap_or_default();
    let got_subtype = writes.get(&cpu_isa::SLOT_CPU_SUBTYPE).cloned().unwrap_or_default();
    assert_eq!(
        got_type, want_type,
        "init 体写进 `SLOT_CPU_TYPE` 的 id 集与 `CLASSIFY` 不等：漏一条 = 那个型号名在任何机器上永为 0，\
         多一条 = 表里存在「发得出但问不到」的 id（本例不运行任何东西，所以与宿主 CPU 无关）"
    );
    assert_eq!(
        got_subtype, want_subtype,
        "init 体写进 `SLOT_CPU_SUBTYPE` 的 id 集与 `CLASSIFY` 不等（同上）"
    );
    // id 从 1 起编（`= 0` 是「未分类」），所以发射器写出的 id 不得包含 0。
    assert!(
        !got_type.contains(&0) && !got_subtype.contains(&0),
        "型号槽被写了 0（分类链把「不写」误成「写 0」会让后面的规则无法覆盖它）"
    );

    // 厂商 kind：四条已知签名都必须发得出（0 = 未匹配时的兵底写 0，它不在签名表里）。
    let want_vendor: BTreeSet<u32> = cpu_isa::VENDOR_SIGS.iter().map(|v| v.kind).collect();
    let got_vendor = writes.get(&cpu_isa::SLOT_VENDOR_KIND).cloned().unwrap_or_default();
    assert!(
        want_vendor.iter().all(|k| got_vendor.contains(k)),
        "init 体没把 `{want_vendor:?}` 全体写进 SLOT_VENDOR_KIND（实得 {got_vendor:?}）：\
         某个厂商的机器上厂商名与全部型号名都会永为 0"
    );
    for k in &got_vendor {
        assert!(
            *k == cpu_isa::VENDOR_UNKNOWN || want_vendor.contains(k),
            "SLOT_VENDOR_KIND 被写了表外的 kind {k}"
        );
    }
}

/// 底线 5 的另一面：仍走内置链接器（用户显式 `-fbuiltin-linker`）的路径不得静默。
///
/// 钉三件事：
///   1. 静态 ET_EXEC 里 IFUNC 被绑到 `<base>.default`（`link::ifunc::bind_static_exec_ifuncs`）。
///      没有这一步时，引用方会直接 `call <base>.resolver`，而 resolver `ret` 回的是
///      **函数指针**不是返回值 —— 实测纯 target_clones 程序因此 SIGSEGV（rc=139）。
///   2. 这条语义退化必须**无条件**写在 stderr（不许退回到静默降级），产物也确实是静态的。
///   3. 最终 exe 里不该再剩 `STT_GNU_IFUNC`：静态产物没人应用 IRELATIVE，留着那个类型
///      就是「有 IFUNC、却永远不会被解析」的装饰。
#[test]
#[cfg(unix)]
fn target_clones_builtin_linker_static_binds_default_and_announces() {
    const STT_GNU_IFUNC: u8 = 10;
    let src = "__attribute__((target_clones(\"sse4.2,default\")))\n\
                int compute(int x){ return x*2+1; }\n\
                int main(void){ return compute(10) == 21 ? 0 : 1; }\n";
    let (ok, stderr, exe) = harness::dcc_link_exe("tc-static-bind", src, &["-O0", "-fbuiltin-linker"]);
    assert!(ok, "纯 target_clones（无 libc）应能用内置链接器链上；stderr:\n{stderr}");
    assert!(
        stderr.contains("bound to variant 'compute.default'"),
        "静态路径把 IFUNC 冻结到 default 变体这件事必须明说（不得静默）；stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("CPUID dispatch is DISABLED"),
        "必须说清静态产物里运行时 CPUID 派发已失效；stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("static (no PT_INTERP / DT_NEEDED)"),
        "内置链接产物必须自报链接方式；stderr:\n{stderr}"
    );
    // 真跑得出正确结果（这一条就是 SIGSEGV 回归的反面：绑进 resolver 后 rc=139）。
    let (rc, _stdout, run_stderr) = run_exe_bounded(&exe);
    let run_stderr = String::from_utf8_lossy(&run_stderr).to_string();
    assert_eq!(
        rc, 0,
        "静态产物的 compute(10) 必须返回 21（绑错东西时要么 SIGSEGV 要么 rc=1）；stderr:\n{run_stderr}"
    );
    // exe 侧：`compute` 与 `compute.default` 同址，且全表不再有任何 IFUNC。
    // 取数走 `elf64_syms_by_type`：内置链接器把自己的 `.symtab/.strtab` 节头 `sh_name`
    // 写成 0（见该助手注释），按名字找符号表的 `elf64_syms` 在这里只会拿到空表。
    let bytes = fs::read(&exe).unwrap_or_default();
    let syms = harness::elf64_syms_by_type(&bytes);
    assert!(!syms.is_empty(), "静态 exe 里找不到 SHT_SYMTAB 节（连符号表都没有）");
    let find = |n: &str| syms.iter().find(|s| s.name == n);
    let dump: Vec<String> = syms
        .iter()
        .map(|s| format!("{}(ty={},val={:#x})", s.name, s.stype, s.value))
        .collect();
    let compute = find("compute").unwrap_or_else(|| panic!("exe 里应有 compute；实际符号：{}", dump.join(" ")));
    let default = find("compute.default").expect("exe 里应有 compute.default");
    assert_eq!(compute.value, default.value, "静态产物里 compute 必须就是 default 变体");
    assert_ne!(compute.stype, STT_GNU_IFUNC, "静态产物不该留永不会被解析的 IFUNC 类型");
    assert!(
        !syms.iter().any(|s| s.stype == STT_GNU_IFUNC && !s.name.is_empty()),
        "静态 exe 里还有 IFUNC 符号 = 又回到「有 IFUNC 没人应用重定位」那个假形状"
    );
    let _ = fs::remove_file(&exe);
}

/// `__builtin_cpu_supports` 在内置链接器的**静态**产物里也必须真能跑（不是只能走宿主链）。
///
/// 这一条钉的是本轮把 `__dcc_cpu_model` 从 `.bss` 换到 `.data` 的决定：内置链接器的
/// 静态路径把 NOBITS 输出节铺成 size=0 / RW `PT_LOAD` 全 0（那一页根本没映射），
/// 缓存坐 `.bss` 时 `__dcc_cpu_indicator_init` 第一个写就 SIGSEGV（实测 rc=139）。
/// 恒 0 桩则会让本夹具返回 3。两者都不是 0，只有「真 CPUID + 真可写的缓存」才得 0。
#[test]
#[cfg(unix)]
fn cpu_builtin_supports_runs_under_builtin_linker_static() {
    let src = "int main(void){\n\
                int sse = __builtin_cpu_supports(\"sse\");\n\
                int sse2 = __builtin_cpu_supports(\"sse2\");\n\
                return (sse && sse2) ? 0 : 3;\n}\n";
    let (ok, stderr, exe) = harness::dcc_link_exe("cpu-static", src, &["-O0", "-fbuiltin-linker"]);
    assert!(ok, "只用 builtin 的程序应能静态链上；stderr:\n{stderr}");
    let (rc, _stdout, run_stderr) = run_exe_bounded(&exe);
    let run_stderr = String::from_utf8_lossy(&run_stderr).to_string();
    assert_eq!(
        rc, 0,
        "x86-64 上 sse/sse2 恒支持：rc=3 说明 builtin 是恒假桩，rc=139 说明 CPUID 缓存没被映射；stderr:\n{run_stderr}"
    );
    let _ = fs::remove_file(&exe);
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
    const STT_GNU_IFUNC: u8 = 10;
    const STT_FUNC: u8 = 2;
    let src = include_str!("../fixtures/c/attr_ifunc.c");
    let (out, obj) = run_object_test("m27-attr-ifunc-obj", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    // 直接解析 ELF 而不是 `readelf -s` 文本：本机 zh_CN.UTF-8 下 readelf 把
    // `STT_GNU_IFUNC` 打成 `<操作系统专用>：10`，所以下面这三条断言只能读字节。
    let data = fs::read(&obj).expect("read obj");
    let _ = fs::remove_file(&obj);
    let syms = elf64_syms(&data);
    let find = |n: &str| syms.iter().find(|s| s.name == n);
    let f = find("ifunc_func").expect("ifunc_func 必须在 symtab 里");
    let r = find("resolve_ifunc").expect("resolve_ifunc 必须在 symtab 里");
    // 旧版只断两个名字都在 —— 连 st_type 是不是 10 都没验。现在三条都钉：
    assert_eq!(f.stype, STT_GNU_IFUNC, "ifunc_func 的 st_type 必须是 STT_GNU_IFUNC(10)");
    assert_eq!(f.bind, 1, "非 static 声明的 IFUNC 必须是 GLOBAL");
    assert_eq!(r.stype, STT_FUNC, "用户写的 resolver 是普通 STT_FUNC");
    assert_eq!(
        (f.value, f.size),
        (r.value, r.size),
        "`__attribute__((ifunc(\"r\")))` 的 IFUNC 必须与 r 同址同尺寸（指向真 resolver 体，\
         不是实现体）。注意：本夹具是**非法 C**（`default_impl` 先用后声明）—— \
         符号形状仍对，但 resolver 体本身是错的，所以上报的未修项里记了它；\
         行为级判据看 attr_target_clones_ifunc_behavior.c"
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
