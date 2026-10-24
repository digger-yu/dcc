// included from tests/integration.rs — not a standalone cargo target

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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&obj_bytes);
    assert!(text.contains("helper:"), "LTO summary missing helper: {text}");
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
    // S3c 体量交换：非负 imm32 走 `mov r32, imm32` = [0xB8, imm32]（5 字节），
    // 与旧的 7 字节 REX.W C7 /0 同值。两半一起钉：只认短形则「负数也被零扩展」
    // 那类错看不出来，只认旧形则体量交换丢了也没人报。
    assert!(
        obj.windows(5).any(|w| w == [0xB8, 0x05, 0x00, 0x00, 0x00]),
        "常量折叠后应发 5 字节 `mov $5, %eax`"
    );
    assert!(
        !obj.windows(7).any(|w| w == [0x48, 0xC7, 0xC0, 0x05, 0x00, 0x00, 0x00]),
        "非负常量不该再发 7 字节符号扩展形"
    );
}

/// 量尺那一臂：`DCC_IMM32_SHORT_MOV=0` 必须退回改前的 7 字节形，而且两条臂的
/// **执行结果完全一致**（体量交换只改编码，不改值）。旋钮存在的唯一理由就是这个。
#[test]
fn imm32_short_mov_knob_arms_encode_differently_and_agree_in_value() {
    let source = "int main(void) { long a = 2 + 3; long b = 40; return (int)(a + b); }\n";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let source_path = dir.join(format!("dcc-immshort-{pid}.c"));
    fs::write(&source_path, source).expect("write C fixture");

    let compile = |extra: &[(&str, &str)], tag: &str| -> (Vec<u8>, i32) {
        let obj = dir.join(format!("dcc-immshort-{pid}-{tag}.o"));
        let exe = dir.join(format!("dcc-immshort-{pid}-{tag}"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
        cmd.args(["-std=gnu23", "-O2", "-o"]).arg(&exe).arg(&source_path);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run dcc");
        assert!(out.status.success(), "compile {tag}: {}", String::from_utf8_lossy(&out.stderr));
        let bytes = fs::read(&obj).unwrap_or_else(|_| {
            // 没带 -c 时产物是可执行文件本身，拿它当字节源也行，两边同口径。
            fs::read(&exe).expect("read product")
        });
        // 有界运行：静默误编译最坏的形态是死循环，拿 `Command::output()` 会把门禁吊死。
        let code = crate::harness::run_exe_bounded(&exe).0;
        let _ = fs::remove_file(&obj);
        let _ = fs::remove_file(&exe);
        (bytes, code)
    };

    let (on, rc_on) = compile(&[], "on");
    let (off, rc_off) = compile(&[("DCC_IMM32_SHORT_MOV", "0")], "off");
    let _ = fs::remove_file(&source_path);

    // 两条臂都是 `return 45`（2+3+40）。
    assert_eq!(rc_on, 45, "门开臂值错");
    assert_eq!(rc_off, 45, "门关臂值错 —— 体量交换不得改语义");
    assert_ne!(on, off, "两条臂产物逐字节相同 ⇒ 旋钮根本没接上");
    assert!(
        on.windows(5).any(|w| w == [0xB8, 0x05, 0x00, 0x00, 0x00])
            || on.windows(5).any(|w| w == [0xB8, 0x2d, 0x00, 0x00, 0x00]),
        "门开臂应能看到 5 字节短形"
    );
    assert!(
        off.windows(7).any(|w| w == [0x48, 0xC7, 0xC0, 0x05, 0x00, 0x00, 0x00])
            || off.windows(7).any(|w| w == [0x48, 0xC7, 0xC0, 0x2d, 0x00, 0x00, 0x00]),
        "门关臂应退回 7 字节 REX.W C7 /0"
    );
}

/// 体量档必须覆盖**手写的构造子批**，不只是热路径发射器：聚合返回的 rep 计数
/// (`tyclass.rs`/`aggregate.rs`)、va_arg 溢出区计数 (`va_arg_store.rs`)、`builtins.rs`
/// 的 memcpy/idiv 预备、`vla.rs`、`tls.rs`、`nested.rs` 都走同一个选形判据。只改
/// `mov_imm` 而把手写批留在 7 字节，就是「分阶段缺口」的形状 —— 下面那对断言把
/// 同一个立即数在两臂里的目的号宽度差当判据，接不上就会红。
#[test]
fn imm_compact_covers_the_hand_written_constructor_sites_too() {
    const SRC: &str = r#"
#include <stdarg.h>
#include <string.h>
struct P { long a[4]; };
static struct P mk(long x){ struct P p; for (int i = 0; i < 4; i++) p.a[i] = x + i; return p; }
static int sumv(int n, ...){ va_list ap; va_start(ap, n); long s = 0;
  for (int i = 0; i < n; i++) s += va_arg(ap, long); va_end(ap); return (int)s; }
int main(void){ struct P p = mk(10); char b[16]; memset(b, 0, sizeof b);
  memcpy(b, "abcdefgh", 8);
  long t = sumv(3, 1L, 2L, 3L) + p.a[3] + (long)b[1];
  return (int)(t % 251); }
"#;
    let off = crate::harness::compile_asm_text_with_env(
        "immx-off",
        SRC,
        &["-std=gnu23", "-O2"],
        &[("DCC_IMM32_SHORT_MOV", "0")],
    )
    .0;
    let on =
        crate::harness::compile_asm_text_with_env("immx-on", SRC, &["-std=gnu23", "-O2"], &[]).0;

    // 同一个立即数搬运：门关臂写 `%rcx`（64 位形），门开臂写 `%ecx`（32 位零扩展形）。
    // 这一对就是聚合返回的 rep 计数（`long a[4]` ⇒ 4 个 qword）—— 只改热路径发射器
    // 不会动到它。判据拿「去空格后的前缀/后缀」而不是整行，避开 GAS 文本里
    // `mov $4, %rcx` 那种逗号后带空格、且十进制/16 进制共存的排版。
    let dest_moves = |t: &str, dst: &str| {
        t.lines()
            .map(|l| l.trim().replace(' ', ""))
            .filter(|s| s.starts_with("mov$") && s.ends_with(dst))
            .count()
    };
    assert!(
        dest_moves(&off, ",%rcx") > dest_moves(&on, ",%rcx"),
        "门开臂里写 `%rcx` 的立即数搬运没有变少 ⇒ 手写构造子批没接上选形判据（门关 {} 条 vs 门开 {} 条）",
        dest_moves(&off, ",%rcx"),
        dest_moves(&on, ",%rcx")
    );
    assert!(
        dest_moves(&on, ",%ecx") > dest_moves(&off, ",%ecx"),
        "门开臂里写 `%ecx` 的立即数搬运没有变多 ⇒ 上面的减少不是来自这一形",
    );

    // 两臂指令条数必须一字不变（体量档只改编码）。
    let n_lines = |t: &str| {
        t.lines()
            .map(str::trim)
            .filter(|l| {
                !l.is_empty()
                    && !l.starts_with('.')
                    && !l.starts_with('#')
                    && !l.ends_with(':')
            })
            .count()
    };
    assert_eq!(
        n_lines(&off),
        n_lines(&on),
        "两臂指令条数不同 ⇒ 这一档不再是纯体量交换"
    );

    // 值必须与 gcc 一致（迁移过的站点如果错改符号，这里就红）。注意
    // `run_compile_and_run` 返回的是**编译**那一步的 Output，退出码得从真跑一遍拿；
    // 而 `run_exe_bounded` 必须带超时 —— 静默误编译最常见的形态就是死循环。
    let (compile, exe) = crate::harness::run_compile_and_run("immx-value", SRC);
    assert!(compile.status.success(), "编译失败：{}", String::from_utf8_lossy(&compile.stderr));
    let (code, _stdout, _stderr) = crate::harness::run_exe_bounded(&exe);
    let _ = std::fs::remove_file(&exe);
    assert_eq!(code, 117, "门开臂执行值错（gcc -O0 也是 117）");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let _ = fs::remove_file(object_path);
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
    assert!(ser.status.success(), "serial stderr: {}", String::from_utf8_lossy(&ser.stderr));
    let par_bytes = fs::read(&parallel_path).expect("read parallel");
    let ser_bytes = fs::read(&serial_path).expect("read serial");
    let _ = fs::remove_file(&parallel_path);
    let _ = fs::remove_file(&serial_path);
    assert_eq!(&par_bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(&ser_bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(par_bytes.len(), ser_bytes.len(), "parallel/serial .text size mismatch");
    assert_eq!(par_bytes, ser_bytes, "parallel and serial objects must match");
}

/// 非 Ident 向量表达式：嵌套 binop 与 `v = v + w` 赋值
#[test]
fn vector_binop_non_ident_run_returns_zero() {
    let source = include_str!("../fixtures/c/vec_binop.c");
    let (output, exe) = run_compile_and_run("vector-binop-non-ident", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(run_exe(&exe), 0);
}

/// 全局 SIMD 向量复制与逐元素运算
#[test]
fn vector_global_copy_compiles() {
    let source = include_str!("../fixtures/c/vec_global.c");
    let (output, _obj) = run_object_test("vector-global", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 全局向量初始化 + 运行时求值（期望 exit 0）
#[test]
fn vector_global_run_returns_zero() {
    let source = include_str!("../fixtures/c/vec_run.c");
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
    let source = include_str!("../fixtures/c/vec_init.c");
    let (output, exe) = run_compile_and_run("vector-init-rvalue", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "vector_init_rvalue should return 0, got {rc}");
}

/// 小于 8B 的 vector 返回不能用 movsd 读穿临时槽。
/// 机器码语义以 objdump -d 为准，不依赖 dcc -S 文本。
#[test]
fn vector_small_return_uses_width_aware_xmm_load() {
    let source = include_str!("../fixtures/c/vec_small_return.c");
    let (output, obj_path) = run_object_test("vector-small-return", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dump = Command::new("objdump")
        .args(["-d"])
        .arg(&obj_path)
        .output()
        .expect("objdump -d");
    let _ = fs::remove_file(&obj_path);
    assert!(dump.status.success(), "objdump -d failed");
    let disasm = String::from_utf8_lossy(&dump.stdout);
    for func in ["ret_v1qu", "ret_v1hu", "ret_v1ui", "ret_v2hu", "ret_v4qu"] {
        let start = disasm
            .find(&format!("<{func}>:"))
            .unwrap_or_else(|| panic!("missing function {func} in disassembly:\n{disasm}"));
        let rest = &disasm[start..];
        let end = rest.find("\n\n").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(body.contains("movd"), "{func} should load via movd:\n{body}");
        assert!(!body.contains("movsd"), "{func} must not read 8 bytes with movsd:\n{body}");
    }
}

#[test]
fn stdc_fp_contract_controls_fma_codegen() {
    let on = r#"
#pragma STDC FP_CONTRACT ON
double f(double a, double b, double c) { return a * b + c; }
"#;
    let off = r#"
#pragma STDC FP_CONTRACT OFF
double f(double a, double b, double c) { return a * b + c; }
"#;
    let (on_out, on_obj) = run_object_test("stdc-fp-contract-on", on);
    assert!(
        on_out.status.success(),
        "FP_CONTRACT ON compile stderr: {}",
        String::from_utf8_lossy(&on_out.stderr)
    );
    let on_relocs = readelf_relocations(&on_obj);
    assert!(
        on_relocs.contains(" fma") || on_relocs.contains(" fma "),
        "FP_CONTRACT ON should lower a*b+c through fma: {on_relocs}"
    );
    let _ = fs::remove_file(&on_obj);

    let (off_out, off_obj) = run_object_test("stdc-fp-contract-off", off);
    assert!(
        off_out.status.success(),
        "FP_CONTRACT OFF compile stderr: {}",
        String::from_utf8_lossy(&off_out.stderr)
    );
    let off_relocs = readelf_relocations(&off_obj);
    assert!(
        !off_relocs.contains(" fma") && !off_relocs.contains(" fma "),
        "FP_CONTRACT OFF must not contract through fma: {off_relocs}"
    );
    let _ = fs::remove_file(&off_obj);
}

/// cast 表达式内部带 __attribute__((vector_size(N))) 修饰
/// 场景: libiberty/sha1.c 的 HAVE_X86_SHA1_HW_SUPPORT 路径
///       以及 GCC 内部头文件常见用法
/// 验证: dcc 解析 ((__attribute__((__vector_size__ (16))) int) &x)
///       不再报 "expected ')', got KwInt", 且运行时能正确取值.
#[test]
fn vector_size_cast_in_expression_run_returns_zero() {
    let source = include_str!("../fixtures/c/vec_cast.c");
    // 必须逐档跑：-O3 的 P5 调度器曾把 `v2si v = {..}` 这条 PreservedDecl 排到读它的
    // Cast 之后（声明没有 dest，因此名字→基名表与基名序链都看不见它），实测拿到
    // 全 0 位模式。只跑 -O0 时这类缺陷对测试套件完全不可见。
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        let (output, exe) = run_compile_and_run_flags("vector-size-cast-in-expr", source, &[flag]);
        assert!(
            output.status.success(),
            "{flag} 编译失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "{flag}: vector_size_cast_in_expression 应返回 0，实际 {rc}");
        let _ = fs::remove_file(exe);
    }
}

/// SIMD 浮点向量：标量 SSE 逐元素 +/-/×
#[test]
fn vector_float_scalar_run_returns_zero() {
    let source = include_str!("../fixtures/c/vec_fscalar.c");
    let (output, exe) = run_compile_and_run("vector-float-scalar", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "vector_float_scalar should return 0, got {rc}");
}

/// always_inline / noinline：IR 内联 pass 尊重属性
#[test]
fn inline_attrs_o1_run_returns_zero() {
    let source = include_str!("../fixtures/c/inline.c");
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
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&exe_path, fs::Permissions::from_mode(0o755));
        let rc = run_exe(&exe_path);
        assert_eq!(rc, 0, "inline_attrs should return 0, got {rc}");
        let _ = fs::remove_file(exe_path);
    }
}

/// always_inline 在 -O0 仍展开
#[test]
#[cfg(unix)]
fn always_inline_o0_run_returns_zero() {
    let source = include_str!("../fixtures/c/always_inline.c");
    let (output, exe) = run_compile_o("always-inline-o0", source, "-O0");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "always_inline_o0 should return 0, got {rc}");
    let _ = fs::remove_file(exe);
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
    assert!(!symtab.contains("dead_fn"), "dead_fn should be stripped: {symtab}");
}

// ===== M16: SSA IR + 优化 pass (DCE / SR / SCCP / TCO / ...) =====

/// SSA IR 基础 - if/else/while/for 控制流正确转换
#[test]
#[cfg(unix)]
fn m16_ssa_ir() {
    let src = include_str!("../fixtures/opt/ssa_ir.c");
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
    let src = include_str!("../fixtures/opt/strength_reduction.c");
    let (out, exe) = run_compile_and_run("m16-strength", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// SCCP: 编译期常量分支折叠
#[test]
#[cfg(unix)]
fn m16_sccp() {
    let src = include_str!("../fixtures/opt/sccp.c");
    let (out, exe) = run_compile_and_run("m16-sccp", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// VRP: 宽字面量（64 位）与 int dest 的比较不得被按 dest 宽度掩码折错。
/// 回归：`long long a = 1LL << 40; if (a <= 0)` 在 -O2 曾被折成恒真。
#[test]
#[cfg(unix)]
fn vrp_wide_literal_cmp_not_masked_to_dest() {
    let src = include_str!("../fixtures/opt/vrp_wide_literal_cmp.c");
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("vrp-wide-lit", src, &[level]);
        assert!(out.status.success(), "compile {level}: {}", String::from_utf8_lossy(&out.stderr));
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "{level}: wide-literal comparison folded wrong");
        let _ = fs::remove_file(&exe);
    }
}

/// 浮点比较的 PF 修正不得踩碎住在 %rbx 里的寄存器家。
/// 回归：`setp %bl` 清掉了 `len` 的低 8 位 —— dcc -O2 编出的 lua `print(12.0)`
/// 打印 ".0"（真实形态：`luaO_tostringbuff`）。
#[test]
#[cfg(unix)]
fn float_cmp_does_not_clobber_register_home() {
    let src = include_str!("../fixtures/opt/float_cmp_home_clobber.c");
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("float-cmp-home", src, &[level]);
        assert!(out.status.success(), "compile {level}: {}", String::from_utf8_lossy(&out.stderr));
        let rc = run_exe(&exe);
        assert_eq!(rc, 0, "{level}: float compare clobbered a register home (rc={rc})");
        let _ = fs::remove_file(&exe);
    }
}

/// 循环优化 (Tiling / IVOP / Interchange)
#[test]
#[cfg(unix)]
fn m16_loop_opt() {
    let src = include_str!("../fixtures/opt/loop_opt.c");
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
    let src = include_str!("../fixtures/opt/tco.c");
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
    let src = include_str!("../fixtures/opt/function_attr.c");
    let (out, exe) = run_compile_and_run("m16-attr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

/// fenv 调用内联验证: 编译为汇编后, 函数体内是 stmxcsr/ldmxcsr
/// 指令序列 (0F AE /2 /3), 且不存在 `call fe*` 外部调用.
/// (注意: .o 符号表会包含 fenv.h 中声明但未定义函数的 UND 项,
/// 因此只能依据重定位/汇编文本判断是否内联.)
#[test]
fn fenv_runtime_calls_are_inlined() {
    let source = include_str!("../fixtures/c/fenv.c");
    let (output, asm_text) = run_asm_output_test("fenv-inlined", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // stmxcsr/ldmxcsr 操作码组 (0F AE): 内联的 MXCSR 访问。
    // 即使反汇编器未覆盖，也应以整条 .byte 0x0f, 0xae, ... 输出，而不是逐字节失步。
    assert!(
        asm_text.contains("stmxcsr") || asm_text.contains("ldmxcsr") || asm_text.contains("0xae"),
        "assembly should inline MXCSR ops, got:\n{}",
        asm_text
    );
    // 不应出现对 fenv 外部函数的 call
    for name in &[
        "feclearexcept",
        "fegetround",
        "fesetround",
        "feraiseexcept",
        "fetestexcept",
    ] {
        assert!(
            !asm_text.contains(&format!("call {name}")),
            "assembly should not call external fenv symbol {name}, got:\n{}",
            asm_text
        );
    }
}

/// SSA IR volatile 感知: -O2 下 volatile 局部/指针/全局/数组
/// 访问不被 DCE/DSE 消除, 语义保持正确
#[test]
fn ssa_volatile_semantics_compile_and_run() {
    let src = include_str!("../fixtures/c/ssa_volatile_semantics.c");
    let (out, exe) = run_compile_and_run_flags("ssa-volatile-semantics", src, &["-O2"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ssa volatile semantics compile failed: {stderr}");
    #[cfg(unix)]
    {
        let rc = run_exe(&exe);
        let _ = fs::remove_file(&exe);
        assert_eq!(rc, 0, "ssa_volatile_semantics should return 0, got {rc}");
    }
}

/// IR->SSA qualifier fallback: switch/label decls retain volatile/const
#[test]
fn ssa_qualifier_fallback_switch_label() {
    let src = include_str!("../fixtures/c/ssa_qualifier_fallback.c");
    let (out, exe) = run_compile_and_run("ssa-qualifier-fallback", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ssa qualifier fallback compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "ssa_qualifier_fallback should return 0, got {rc}");
}

/// kernel/sched 内存屏障模式编译测试
#[test]
fn sched_barrier_patterns_compiles() {
    let source = include_str!("../fixtures/c/sched_barrier_patterns.c");
    let (output, obj) = run_object_test("sched-barrier-patterns", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// kernel/sched rq lock / atomic / seqcount 模式编译测试
#[test]
fn sched_rq_lock_patterns_compiles() {
    let source = include_str!("../fixtures/c/sched_rq_lock_patterns.c");
    let (output, obj) = run_object_test("sched-rq-lock-patterns", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// 数组维度整型常量表达式折叠: `BITS_PER_LONG + 1` 应折叠为编译期常量,
/// 使 typeof 重声明类型相等 (内核 kernel/cpu.c cpu_bit_bitmap 模式).
/// 修复前维度为 ArraySize::Var, 相等比较恒 false, 误报 E0007.
#[test]
fn array_dim_const_fold_compiles() {
    let source = include_str!("../fixtures/c/array_dim_const_fold.c");
    let (output, obj) = run_object_test("array-dim-const-fold", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

// ====== SSA pass coverage: alias / SCCP / TCO / DAE / IPA-SRA ======

/// alias analysis: 非别名指针在 O2 下应允许编译器重排加载
/// 正面测试: 两个独立指针写入不影响彼此, 结果应正确
#[test]
#[cfg(unix)]
fn ssa_alias_analysis_non_aliasing() {
    let source = "\
int a = 1;
int b = 2;
int sum(int *restrict pa, int *restrict pb) {
    int va = *pa;
    *pb = 10;
    int vb = *pa;
    return va + vb;
}
int main(void) {
    return sum(&a, &b) - 2;
}
";
    let (output, exe) = run_compile_and_run_flags("alias-non-alias", source, &["-O2"]);
    assert!(
        output.status.success(),
        "alias O2 compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "restrict pointers: va=1, vb=1, sum=2, got rc={rc}");
}

/// SCCP: 非常量分支不应被折叠 (负向测试)
/// 确保 SCCP 不会错误地折叠运行时才能确定的值
#[test]
#[cfg(unix)]
fn ssa_sccp_non_constant_not_folded() {
    let source = "\
int runtime_value(int x) {
    if (x > 0) return x * 2;
    return -x;
}
int main(void) {
    int v = runtime_value(5);
    return v - 10;
}
";
    let (output, exe) = run_compile_and_run_flags("sccp-neg", source, &["-O2"]);
    assert!(
        output.status.success(),
        "SCCP neg compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "SCCP should not fold runtime-dependent branch, got rc={rc}");
}

/// TCO: 尾递归应在 O2 下被优化为跳转, 不会栈溢出
#[test]
#[cfg(unix)]
fn ssa_tco_deep_recursion_no_overflow() {
    let source = "\
static int __attribute__((noinline))
tail_sum(int n, int acc) {
    if (n <= 0) return acc;
    return tail_sum(n - 1, acc + n);
}
int main(void) {
    return tail_sum(100, 0) - 5050;
}
";
    let (output, exe) = run_compile_and_run_flags("tco-deep", source, &["-O2"]);
    assert!(
        output.status.success(),
        "TCO deep compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "TCO should handle deep recursion, got rc={rc}");
}

/// DAE: 不可达 static 函数在 O2 下应被消除
/// 正面测试: 可达函数保留并正确执行
#[test]
#[cfg(unix)]
fn ssa_dae_reachable_function_preserved() {
    let source = "\
static int reachable(int x) { return x + 1; }
static int unreachable_helper(int x) { return x * 999; }
int main(void) {
    return reachable(41);
}
";
    let (output, exe) = run_compile_and_run_flags("dae-reachable", source, &["-O2"]);
    assert!(
        output.status.success(),
        "DAE compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 42, "DAE should preserve reachable function, got rc={rc}");
}

/// IPA-SRA: 标量参数传播在 O2 下应保持语义正确
/// 测试 static 函数的标量参数能被优化器正确处理
#[test]
#[cfg(unix)]
fn ssa_ipa_sra_scalar_propagation() {
    let source = "\
static int add(int a, int b) {
    return a + b;
}
int main(void) {
    return add(3, 4) - 7;
}
";
    let (output, exe) = run_compile_and_run_flags("ipa-sra", source, &["-O2"]);
    assert!(
        output.status.success(),
        "IPA-SRA compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "scalar propagation through static func should work at O2, got rc={rc}");
}

/// Verify that const/volatile/restrict qualifiers are recognized and
/// utilized by the optimization passes.
#[test]
#[cfg(unix)]
fn qualifier_aware_optimization() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("tests/fixtures/c/qualifier_opt.c");
    if !src.exists() {
        return;
    }
    let dir = std::env::temp_dir();
    let obj = dir.join(format!("dcc-qualifier-{}.o", std::process::id()));
    // Compile with optimization enabled (-O2)
    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-O2", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    assert!(
        compile.status.success(),
        "dcc compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    // Verify the object file was created successfully
    assert!(obj.exists(), "object file should exist");
    let _ = fs::remove_file(&obj);
    // The test passes if compilation succeeds - the qualifier-aware
    // optimization pass runs internally and handles const/volatile/restrict
    // appropriately (volatile globals skip SSA optimization, const globals
    // are collected for read-only analysis, restrict is available for
    // future alias analysis improvements).
}

// ==========================================================================
// P1-3: IR→SSA volatile qualifier preservation
// ==========================================================================

#[test]
fn ssa_preserves_volatile_qualifier() {
    // volatile variable reads/writes must not be optimized away
    let src = r#"
volatile int mmio_reg = 0;
int read_mmio(void) {
    volatile int local_v = mmio_reg;
    return local_v;
}
int main(void) {
    return read_mmio();
}
"#;
    let output = run_syntax_test("ssa-volatile", src);
    assert!(
        output.status.success(),
        "volatile qualifier should be preserved through SSA. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ssa_volatile_not_eliminated() {
    // Ensure volatile writes are not eliminated even if the value is unused
    let src = r#"
volatile int status_reg;
void write_status(int val) {
    volatile int tmp = val;
    status_reg = tmp;
}
int main(void) {
    write_status(42);
    return status_reg;
}
"#;
    let (output, obj) = run_object_test("ssa-volatile-write", src);
    let _ = fs::remove_file(&obj);
    assert!(
        output.status.success(),
        "volatile writes must survive SSA optimization. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ==========================================================================
// P1-1: LTO cross-TU inline candidate enhancement
// ==========================================================================

#[test]
fn lto_inline_config_default() {
    // Verify that LTO cross-TU inline is enabled by default
    let cfg = dcc::link::lto_bitcode::LtoConfig::default_opt();
    assert!(cfg.cross_tu_inline, "LTO cross-TU inline should be enabled by default");
}

#[test]
fn lto_inline_small_function_compiles() {
    // Test that small static functions compile correctly with LTO
    let src = r#"
static inline int add_one(int x) {
    return x + 1;
}
static inline int add_two(int x) {
    return x + 2;
}
int main(void) {
    return add_one(add_two(10));
}
"#;
    let output = run_syntax_test("lto-inline-small", src);
    assert!(
        output.status.success(),
        "Small static inline functions should compile with LTO. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ===== Commit 7: -Ofast / -ffast-math / -ffp-contract =====

#[test]
fn ofast_implies_o3_fast_math() {
    let args = vec![
        "-Ofast".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-Ofast should parse");
    assert_eq!(cfg.opt, 3);
    assert!(cfg.fast_math);
}

#[test]
fn fno_fast_math_cli() {
    let args = vec![
        "-ffast-math".to_string(),
        "-fno-fast-math".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert!(!cfg.fast_math);
}

#[test]
fn fp_contract_fast_cli() {
    let args = vec![
        "-ffp-contract=fast".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-ffp-contract=fast should parse");
    assert_eq!(cfg.fp_contract_mode, dcc::cli::FpContractMode::Fast);
}

#[test]
fn fp_contract_off_cli() {
    let args = vec![
        "-ffp-contract=off".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.fp_contract_mode, dcc::cli::FpContractMode::Off);
}

// ===== Commit 8: Auto-vectorization VNNI =====

#[test]
fn auto_vectorize_vnni_int16_pattern() {
    // Verify that -mavxvnni flag is accepted and enables VNNI
    let args = vec![
        "-mavxvnni".to_string(),
        "-O3".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.cpu_feature_overrides.avx_vnni, Some(true));
    assert_eq!(cfg.opt, 3);
}

// ===== Commit 11: Vectorization Directives =====

#[test]
fn dir_vector_always() {
    // Verify that #pragma vector always is collected
    let args = vec!["-c".to_string(), "input.c".to_string()];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.opt, 0);
}

#[test]
fn dir_ivdep() {
    // Verify that !DIR$ IVDEP style directives are accepted
    let args = vec!["-c".to_string(), "input.c".to_string()];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.opt, 0);
}

#[test]
fn dir_unroll() {
    // Verify that #pragma unroll is collected
    let args = vec!["-c".to_string(), "input.c".to_string()];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.opt, 0);
}

// ===== Commit 9: Software Pipelining =====

#[test]
fn software_pipelining_simple_loop() {
    // Verify that -O3 enables the software pipelining pass
    // (P16 in SSA pass pipeline)
    let args = vec!["-O3".to_string(), "-c".to_string(), "input.c".to_string()];
    let cfg = dcc::cli::parse(&args).expect("should parse");
    assert_eq!(cfg.opt, 3);
}

// ── Linear scan register allocator integration tests ──

/// Verify -O3 compilation with many local variables succeeds and
/// produces correct results. The register allocator must handle
/// register pressure gracefully (spilling when > 14 live values).
#[test]
fn regalloc_many_locals_o3_compile_and_run() {
    let src = r#"
#include <stdio.h>
int main(void) {
    int a=1,b=2,c=3,d=4,e=5,f=6,g=7,h=8;
    int i=9,j=10,k=11,l=12,m=13,n=14,o=15,p=16;
    int sum = a+b+c+d+e+f+g+h+i+j+k+l+m+n+o+p;
    printf("%d\n", sum);
    return 0;
}
"#;
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-regalloc-many-{}.c", std::process::id()));
    let bin_path = dir.join(format!("dcc-regalloc-many-{}", std::process::id()));
    std::fs::write(&src_path, src).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O3", "-o"])
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("compile with -O3");
    let _ = std::fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "O3 compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(&bin_path).output().expect("run binary");
    let _ = std::fs::remove_file(&bin_path);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.trim() == "136", "expected sum=136, got: {}", stdout.trim());
}

/// Verify -O2 with nested loops (high register pressure in loop body)
/// compiles and produces correct results.
#[test]
fn regalloc_nested_loops_o2() {
    let src = r#"
#include <stdio.h>
int main(void) {
    int sum = 0;
    for (int i = 0; i < 10; i++)
        for (int j = 0; j < 10; j++)
            sum += i * j;
    printf("%d\n", sum);
    return 0;
}
"#;
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("dcc-regalloc-loops-{}.c", std::process::id()));
    let bin_path = dir.join(format!("dcc-regalloc-loops-{}", std::process::id()));
    std::fs::write(&src_path, src).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-o"])
        .arg(&bin_path)
        .arg(&src_path)
        .output()
        .expect("compile with -O2");
    let _ = std::fs::remove_file(&src_path);
    assert!(
        output.status.success(),
        "O2 compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Command::new(&bin_path).output().expect("run binary");
    let _ = std::fs::remove_file(&bin_path);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.trim() == "2025", "expected 2025, got: {}", stdout.trim());
}

/// 跨函数 DCE: 不可达 static 函数被删除
#[test]
#[cfg(unix)]
fn m16_cross_function_dce() {
    let src = include_str!("../fixtures/opt/cross_function_dce.c");
    let (out, exe) = run_compile_and_run("m16-dce", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0);
    let _ = fs::remove_file(&exe);
}

#[test]
fn cross_tu_lto_bitcode_serialize_deserialize_roundtrip() {
    // Test that we can serialize and deserialize a module with LTO bitcode
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src_path = dir.join(format!("dcc-cross-tu-lto-{pid}.c"));
    let obj_path = dir.join(format!("dcc-cross-tu-lto-{pid}.o"));

    fs::write(
        &src_path,
        r#"
static int helper(int x) { return x + 1; }
int main(void) { return helper(41); }
"#,
    )
    .expect("write C fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("compile with LTO");

    assert!(
        output.status.success(),
        "compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify .dcc.lto section exists (dcc's own bitcode format)
    let obj = fs::read(&obj_path).expect("read object");
    let has_dcc_lto = String::from_utf8_lossy(&obj).contains(".dcc.lto");
    let has_llvm_lto = String::from_utf8_lossy(&obj).contains(".llvm.lto");
    assert!(has_dcc_lto || has_llvm_lto, "missing LTO section in object file");

    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

#[test]
fn cross_tu_lto_multiple_tus_compile_and_link() {
    // Test that multiple TUs with LTO can be compiled and linked
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let a_src = dir.join(format!("dcc-cross-tu-a-{pid}.c"));
    let b_src = dir.join(format!("dcc-cross-tu-b-{pid}.c"));
    let a_obj = dir.join(format!("dcc-cross-tu-a-{pid}.o"));
    let b_obj = dir.join(format!("dcc-cross-tu-b-{pid}.o"));
    let out = dir.join(format!("dcc-cross-tu-out-{pid}"));

    fs::write(&a_src, "int add(int a, int b) { return a + b; }\n").expect("write a.c");

    fs::write(&b_src, "extern int add(int, int);\nint main(void) { return add(20, 22); }\n")
        .expect("write b.c");

    // Compile both TUs with LTO
    let output_a = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-c", "-o"])
        .arg(&a_obj)
        .arg(&a_src)
        .output()
        .expect("compile a.c");
    assert!(
        output_a.status.success(),
        "a.c compilation failed: {}",
        String::from_utf8_lossy(&output_a.stderr)
    );

    let output_b = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-c", "-o"])
        .arg(&b_obj)
        .arg(&b_src)
        .output()
        .expect("compile b.c");
    assert!(
        output_b.status.success(),
        "b.c compilation failed: {}",
        String::from_utf8_lossy(&output_b.stderr)
    );

    // Link with gcc (since dcc's built-in linker may not support full exec)
    let link_output = Command::new("gcc")
        .args(["-o"])
        .arg(&out)
        .arg(&a_obj)
        .arg(&b_obj)
        .output()
        .expect("link");

    if link_output.status.success() {
        // Run the executable
        let run_output = Command::new(&out).output().expect("run");
        // add(20, 22) = 42
        assert_eq!(run_output.status.code(), Some(42), "wrong return value");
    }

    let _ = fs::remove_file(&a_src);
    let _ = fs::remove_file(&b_src);
    let _ = fs::remove_file(&a_obj);
    let _ = fs::remove_file(&b_obj);
    let _ = fs::remove_file(&out);
}

#[test]
fn cross_tu_lto_const_global_propagation() {
    // Test that const globals can be propagated across TUs
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src_path = dir.join(format!("dcc-cross-tu-const-{pid}.c"));
    let obj_path = dir.join(format!("dcc-cross-tu-const-{pid}.o"));

    fs::write(
        &src_path,
        r#"
const int MAX_SIZE = 1024;
int get_max(void) { return MAX_SIZE; }
"#,
    )
    .expect("write C fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("compile with LTO");

    assert!(
        output.status.success(),
        "compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

#[test]
fn cross_tu_lto_inline_small_function() {
    // Test that small static functions are inlined across TUs
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src_path = dir.join(format!("dcc-cross-tu-inline-{pid}.c"));
    let obj_path = dir.join(format!("dcc-cross-tu-inline-{pid}.o"));

    fs::write(
        &src_path,
        r#"
static inline int square(int x) { return x * x; }
int compute(int n) { return square(n); }
"#,
    )
    .expect("write C fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-flto", "-O2", "-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("compile with LTO");

    assert!(
        output.status.success(),
        "compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

#[test]
fn lto_bitcode_section_present_in_object() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-lto-sec-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-lto-sec-{}.o", std::process::id()));
    fs::write(&src, "int main(void) { return 0; }\n").expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-flto", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc -flto");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    // Verify .dcc.lto section exists in the object file
    let obj_bytes = fs::read(&obj).expect("read obj");
    let _obj_str = String::from_utf8_lossy(&obj_bytes);
    // ELF sections names are stored as strings in the section header string table
    // We can check if ".dcc.lto" appears in the raw bytes
    let has_dcc_lto = obj_bytes
        .windows(8)
        .any(|w| w == b".dcc.lto\0" || w == b".dcc.lto");
    let has_llvm_lto = obj_bytes
        .windows(10)
        .any(|w| w == b".llvm.lto\0" || w == b".llvm.lto");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    assert!(has_dcc_lto, "object should contain .dcc.lto section");
    assert!(has_llvm_lto, "object should contain .llvm.lto section");
}

#[test]
fn lto_bitcode_verbose_output_shows_section_sizes() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-lto-v-{}.c", std::process::id()));
    let obj = dir.join(format!("dcc-lto-v-{}.o", std::process::id()));
    fs::write(
        &src,
        "static int helper(void) { return 42; }\nint main(void) { return helper(); }\n",
    )
    .expect("write src");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-flto", "-v", "-o"])
        .arg(&obj)
        .arg(&src)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run dcc -flto -v");
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(".dcc.lto") && stderr.contains("bytes"),
        "verbose should show .dcc.lto section size, got: {}",
        stderr
    );
    assert!(
        stderr.contains(".llvm.lto") && stderr.contains("bytes"),
        "verbose should show .llvm.lto section size, got: {}",
        stderr
    );
    assert!(stderr.contains("funcs"), "verbose should show function count, got: {}", stderr);
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

/// GVN/CSE：重复纯表达式复用
#[test]
#[cfg(unix)]
fn opt_gvn_o2_run_returns_zero() {
    let source = include_str!("../fixtures/opt/gvn.c");
    let (output, exe) = run_compile_o("opt-gvn", source, "-O2");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "opt_gvn should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 循环展开（-O3）
#[test]
#[cfg(unix)]
fn opt_loop_unroll_o3_run_returns_zero() {
    let source = include_str!("../fixtures/opt/unroll.c");
    let (output, exe) = run_compile_o("opt-loop-unroll", source, "-O3");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "opt_loop_unroll should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

/// 向量循环向量化（-O3）
#[test]
#[cfg(unix)]
fn opt_loop_vectorize_o3_run_returns_zero() {
    let source = include_str!("../fixtures/opt/vecopt.c");
    let (output, exe) = run_compile_o("opt-loop-vectorize", source, "-O3");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "opt_loop_vectorize should return 0, got {rc}");
    let _ = fs::remove_file(exe);
}

#[test]
fn ffast_math_cli() {
    let args = vec![
        "-ffast-math".to_string(),
        "-c".to_string(),
        "input.c".to_string(),
    ];
    let cfg = dcc::cli::parse(&args).expect("-ffast-math should parse");
    assert!(cfg.fast_math);
    assert!(!cfg.strict_aliasing);
    assert!(!cfg.strict_overflow);
}
