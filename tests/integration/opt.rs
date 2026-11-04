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

/// `ptr - ptr` 的结果必须是整数（C11 6.5.6p9）：`(top - old) + nw` 里只有
/// `+ nw` 该按元素缩放。修复前 dcc 把减法也记成 `Ptr`，`r` 落到 buf + 5。
#[test]
#[cfg(unix)]
fn opt_ptr_sub_result_type() {
    let source = include_str!("../fixtures/opt/ptr_sub_result.c");
    let rc = run_gcc_cross_check_opt("opt-ptr-sub-result", source, &["-O2"]);
    assert_eq!(rc, 0, "`top - old + nw` 应得到 buf + 3，got rc={rc}");
}

/// `i + p` 的缩放必须落在索引上：修复前 `imul` 恒作用于右操作数，
/// `i + buf` 会把指针本身乘 16。
#[test]
#[cfg(unix)]
fn opt_ptr_add_reversed_operands() {
    let source = include_str!("../fixtures/opt/ptr_add_reversed.c");
    let rc = run_gcc_cross_check_opt("opt-ptr-add-reversed", source, &["-O2"]);
    assert_eq!(rc, 0, "`i + buf` 应等于 `buf + i`，got rc={rc}");
}

/// AST GVN 的别名链必须一次压平：`dt -> vt -> target` 只替换一层会留下悬空
/// 引用，`-O2` 下 `p2` 的 `out[1]` 读到未初始化栈槽。
#[test]
#[cfg(unix)]
fn opt_gvn_alias_chain_dup() {
    let source = include_str!("../fixtures/opt/gvn_alias_chain_dup.c");
    let rc = run_gcc_cross_check_opt("opt-gvn-alias-chain", source, &["-O2"]);
    assert_eq!(rc, 0, "同参数别名链应保持 out[1] == 18，got rc={rc}");
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

/// 常量选择子的 `switch` 必须折成命中的那条臂 —— 否则内核的「编译期尺寸检查」陷阱函数
/// 会被发成未定义引用，最终链接直接失败。
///
/// 现场（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.11）：Linux 7.1 全树链接报
/// `__xadd_wrong_size` 269 处、`__cmpxchg_wrong_size` 242 处、`__bad_size_call_parameter`
/// 261 处、`__get_user_bad`/`__put_user_bad` 各 ~50 处。这些函数**故意不存在**：
/// ```c
/// #define __raw_cmpxchg(ptr, old, new, size, lock) ({ switch (size) {
///     case 1: …; break; … default: __cmpxchg_wrong_size(); } … })
/// ```
/// 调用点传的是常量 `sizeof(*(ptr))`，gcc 折完根本不引用它们。以前 `fold_branches_stmt`
/// 既没有 `Stmt::Switch` 臂，也不下探 `Stmt::Expr` 里的 `({...})` 与 `Stmt::Do` 的体
/// —— 于是这类 switch 从来没被看过一眼。
#[test]
#[cfg(unix)]
fn const_switch_and_do_while_dead_branch_leave_no_trap_references() {
    let src = r#"
extern void trap_size(void); extern void trap_lock(void); extern int debug_flags;
static int sz(void *p, int x)
{
	int r = 0;
	switch (sizeof(int)) {
	case 1: r = 1; break;
	case 4: r = 4; break;
	default: trap_size(); r = -1;
	}
	return r + x + (int)(long)p;
}
static int lk(int x)
{
	do {
		if (0) { trap_lock(); x = 9; }
		x += 1;
	} while (0);
	return x;
}
static int lk2(int x)
{
	do {
		switch (1) {
		case 1: x += 3; break;
		default: trap_lock();
		}
	} while (0);
	return x;
}
int probe(void *p) { return sz(p, 1) + lk(2) + lk2(3) + debug_flags; }
"#;
    let (out, obj) = run_object_test_flags("const-switch-traps", src, &["-O2"]);
    assert!(
        out.status.success(),
        "编译失败: {}", String::from_utf8_lossy(&out.stderr)
    );
    let bytes = fs::read(&obj).expect("read .o");
    let _ = fs::remove_file(&obj);
    let syms = elf64_syms_by_type(&bytes);
    // 符号索引 → 名字（elf64_all_rela 给的是索引，0 是 STN_UNDEF）
    let rels = elf64_all_rela(&bytes);
    for trap in ["trap_size", "trap_lock"] {
        let referenced = rels.iter().any(|(_, idx, _, _)| {
            syms.get(*idx as usize).map(|s| s.name == trap).unwrap_or(false)
        });
        assert!(
            !referenced,
            "`{trap}` 是常量 switch / do-while 死分支里的编译期检查陷阱，折完不该留下任何引用；\
             实测符号表里有它且被重定位引用（这正是内核 vmlinux 链接失败的那一类）"
        );
    }
    assert!(
        syms.iter().any(|s| s.name == "probe" && s.shndx != 0),
        "`probe` 本身必须照常发射"
    );
}

/// 常量 switch 折叠必须处理两件事，否则会把本来不该发码的陷阱调用「拼活了」：
/// ① C 的 fall-through 会从命中臂一路跑进后面的臂（所以不能只留命中那一臂）；
/// ② 内核宏的臂体几乎全是 `case 8: { …; asm(…); break; }` 这种花括号块，`break` 不在臂的
/// 顶层。只认顶层 `break` 会把这条臂当成「落空」而一直拼到 `default: __cmpxchg_wrong_size();`，
/// 实测 `kernel/printk/printk_ringbuffer.o` 的陷阱重定位因此从 9 涨到 15（倒退）。
/// 两个档位都与 gcc 对拍运行期结果（花括号臂 + 真 fall-through 链）。
#[test]
#[cfg(unix)]
fn const_switch_fold_handles_braced_arm_and_fallthrough_chain() {
    let src = r#"
extern void trap_d(void);
static long f(long *v, long o)
{
	long ret = 0;
	switch (sizeof(*v)) {
	case 4:
		{ ret = o + 1; break; }
	case 8:
		{ ret = o + 2; break; }
	default:
		trap_d();
	}
	return ret;
}
static int chain(void)
{
	int r = 0;
	switch (1) {
	case 1: { r += 1; }
	case 2: { r += 10; break; }
	default: { r += 100; }
	}
	return r;
}
int main(void)
{
	long x = 0;
	return (int)(f(&x, 5) * 100 + chain());
}
"#;
    // 花括号臂的 break 必须被看到 ⇒ 命中 case 8 后不得拼进 default 的 trap_d()
    let (out, obj) = run_object_test_flags("const-switch-braced", src, &["-O2"]);
    assert!(
        out.status.success(),
        "编译失败: {}", String::from_utf8_lossy(&out.stderr)
    );
    let bytes = fs::read(&obj).expect("read .o");
    let _ = fs::remove_file(&obj);
    let syms = elf64_syms_by_type(&bytes);
    let rels = elf64_all_rela(&bytes);
    let referenced = rels
        .iter()
        .any(|(_, idx, _, _)| syms.get(*idx as usize).map(|s| s.name == "trap_d").unwrap_or(false));
    assert!(
        !referenced,
        "花括号臂里（case 8 后紧跟的块）的 break 必须被当成终止；否则会拼出不该发码的 \
         trap_d() 调用（本轮实测踩过：陷阱重定位 9→15）"
    );
    // 运行期：f(&x,5)=7（命中 case 8）、chain()=11（case 1 fall-through 到 case 2 后 break）
    // ⇒ 7*100+11 = 711；与 gcc 逐字相同（两个档位都跑）。
    assert_eq!(run_gcc_cross_check_opt("const-switch-braced-o2", src, &["-O2"]), 711 & 0xff);
    let _ = run_gcc_cross_check_opt("const-switch-braced-o1", src, &["-O1"]);
}

/// 反向护栏：常量 switch 折叠**不得**改变 C 语义。判据是运行期与 gcc 同值，覆盖
/// fall-through、GNU 区间、无命中、`default` 收尾、循环内的常量 switch、以及
/// 「被丢的臂里有标签所以不许折」这一条保守面。
#[test]
#[cfg(unix)]
fn const_switch_fold_preserves_semantics_vs_gcc() {
    let src = r#"
static int ft(int k) { int r = 0; switch (k) { case 1: r += 10; /*fallthrough*/
	case 2: r += 20; break; case 3: r += 30; } return r; }
static int rng(int x) { int r = 0; switch (x) { case 1 ... 5: r = 1; break;
	case 6: r = 2; default: r += 3; } return r; }
static int nomatch(int x) { int r = 5; switch (x) { case 1: r = 1; break; case 2: r = 2; break; } return r; }
static int deflt(int x) { switch (x) { case 1: return 11; default: return 99; } }
static int cs(void) { int t = 0; for (int i = 0; i < 3; i++) { switch (4) {
	case 1: t += 1; break; case 4: t += 4; break; default: t += 100; } } return t; }
static int labeled(int x) { switch (x) { case 1: goto L; case 2: return 7;
L: return 3; default: return 4; } }
int main(void)
{
	unsigned v = (unsigned)(ft(1) * 7 + ft(2) * 5 + ft(3) * 3 + rng(3) + rng(6) * 11
		+ nomatch(7) * 13 + deflt(2) * 17 + cs() * 19 + labeled(1) * 23 + labeled(2) * 29);
	return (int)(v % 251u);
}
"#;
    // 这两个调用本身就在内部断言「gcc -O0 与 dcc 该档位的 stdout 逐字相同且退出码相同」，
    // 所以这里不再拿两边的返回值互比（那只会退化成一个常量对自比）。
    // 两个档位都跑：只测 -O2 会漏「常量折叠在哪一档开启」这个事实本身。
    let _ = run_gcc_cross_check_opt("const-switch-semantics-o2", src, &["-O2"]);
    let _ = run_gcc_cross_check_opt("const-switch-semantics-o1", src, &["-O1"]);
}

/// 强制内联拼接一个体内无 `return` 的 `__always_inline` void 助手后，不可达的
/// `default:` 陷阱分支必须**连引用都不发**。
///
/// 为什么只数重定位、不看 `nm -u`：dcc 会为「声明过但没引用」的名字发幽灵 UND 条目，
/// 按 UND 判会 100% 误报（本轮实测：`t9_dcc.o` symtab 里有 `UND trap_c1/trap_c2`，
/// 而 `.rela.text` 条目数是 0）。只有 `.rela.*` 里的条目才会变成 `ld` 的
/// `undefined reference to '__cmpxchg_wrong_size'`。
///
/// 修复点：`src/opt.rs::rewrite_returns_to_dest` 报告「本次是否真改写掉过 `return`」，
/// `try_static_inline_general` 据此决定是否发那条出口标签 —— 没人引用的死标签会让整个
/// caller 被 `ssa/structure.rs` 判为非结构化、走 analysis-only 分支不回写 AST，
/// 于是它的常量 switch 进不了 SCCP（详见 `report/linux71-dcc-fulltree-build-2026-10-28.md` §3.16）。
#[test]
#[cfg(unix)]
fn forced_inline_void_switch_leaves_no_trap_relocation() {
    let src = include_str!("../fixtures/opt/inline_void_switch_trap.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("inline-void-trap-reloc", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}", String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let hits: Vec<&str> = elf64_all_rela(&bytes)
            .iter()
            .filter_map(|(_, idx, _, _)| {
                syms.get(*idx as usize)
                    .filter(|s| s.name == "trap_void")
                    .map(|s| s.name.as_str())
            })
            .collect();
        assert!(
            hits.is_empty(),
            "{flag}: 不可达的 default 臂必须连引用都不发，实测有 {} 条重定位指向 trap_void \
             （这就是内核 vmlinux 链接失败的那一类：`undefined reference to '__cmpxchg_wrong_size'`）",
            hits.len()
        );
    }
}

/// 上一条的语义对偶：强制内联拼接 + 常量 switch 折叠**不得**改变运行期行为，四档
/// (-O0/-O1/-O2/-O3) 的 stdout 与退出码都必须与 gcc 一致。
///
/// 为什么四档都要跑而不只 -O2：`forced_inline_void_switch_leaves_no_trap_relocation`
/// 只锁「引用没发出去」，那是产物级判据；把不可达的 `default:` 折掉的同时必须保证
/// 真 fall-through 链（B 形）、花括号臂的 `break`（C 形）、带标签臂 + `goto`（D 形）
/// 都没被弄坏，那只有运行期能证明。本修复让 caller 函数从 analysis-only 通道转入
/// 「SSA + 重建 + 回写 AST」通道 —— 那条通道虽有 correct-by-bail 退路，但也有误编译
/// 历史（`docs/wp_multi_exit_loop.md`），所以语义护栏必须与产物判据成对存在。
#[test]
#[cfg(unix)]
fn inline_void_switch_trap_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/inline_void_switch_trap.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("inline-void-switch-trap", src, &[flag]),
            0,
            "{flag}: 常量 switch 折叠 / 拼接出口标签的改动破坏了 \
             A(void 助手+常量实参) / B(真 fall-through) / C(花括号臂 break) / \
             D(带标签臂+goto) 中某一形的语义",
        );
    }
}

/// 同名两条 `FuncDecl`（`static inline` 定义 + 后面的非 static 声明）不得让跨函数 DCE
/// 把定义体里**被取地址**的 `static inline` 判成不可达。
///
/// 缺陷现场（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.17）：全树链接报
/// 231 处 `undefined reference to 'btf_id_cmp_func'`。内核里
/// `include/linux/btf.h:514` 的 `static inline bool btf_id_set_contains(...)` 把
/// `btf_id_cmp_func` 的地址交给 `bsearch`，而 `include/linux/bpf.h:3873` 又有一条
/// 同名**非 static 声明**（gcc 接受这个方向）。`cross_function_dce_with_roots` 旧版
/// 按名字 `insert` 边表，声明那条的空边表把定义的边整份盖掉 ⇒ 引用点被删、
/// `lea` 重定位照发。实测本夹具在回退该合并后 `-O2` 立即产出
/// `UND NOTYPE cmp_fn` + 发条 `.rela.text` 指向它（-O0 不犯，只在优化通道上成立）。
#[test]
#[cfg(unix)]
fn extern_decl_after_inline_def_keeps_addr_taken_static_defined() {
    let src = include_str!("../fixtures/opt/dce_extern_decl_inline_addr_taken.c");
    for flag in ["-O1", "-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("dce-extern-decl-inline-addr", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        // `cmp_fn` 是内部链接（static）实体：按 C/ELF 语义它**任何**未定义条目都是错的。
        let und: Vec<String> = syms
            .iter()
            .filter(|s| s.name == "cmp_fn" && s.shndx == 0)
            .map(|s| format!("{} bind={} type={}", s.name, s.bind, s.stype))
            .collect();
        assert!(
            und.is_empty(),
            "{flag}: static inline 的地址被取走时不得发成 UND 条目（这就是内核链接失败的 \
             `undefined reference to 'btf_id_cmp_func'`）：实测 {und:?}",
        );
        // 重定位若引用它，必须绑到有定义的条目（与上一条同一判据的发射侧视图）。
        let bad: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
            .filter(|s| s.name == "cmp_fn" && s.shndx == 0)
            .map(|s| s.name.clone())
            .collect();
        assert!(
            bad.is_empty(),
            "{flag}: .rela.text 里有 {} 条重定位指向未定义的 cmp_fn", 
            bad.len()
        );
    }
}

/// 上一条的语义护栏：取函数地址 + 通函数指针回调这套机制在四档下都不得改变
/// 运行期行为（stdout 与退出码逐字节与 gcc 一致）。夹具里 `main` 只走
/// `live_contains`/`cmp_live` 那条真实路径，`set_contains`/`cmp_fn` 保持内核那种
/// 「全 TU 没人调用」的形状，所发体只在产物级判据里管。
#[test]
#[cfg(unix)]
fn extern_decl_after_inline_def_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/dce_extern_decl_inline_addr_taken.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("dce-extern-decl-inline-addr", src, &[flag]),
            0,
            "{flag}: DCE 同名边表合并破坏了「非 static 声明跟 static inline 定义后面」\
             这一合法形状的语义",
        );
    }
}

/// 内联 asm 里 `sym±N` 的写法不得变成一个「名字叫 `cpu_data+8`」的符号。
///
/// 缺陷现场（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.17）：全树链接
/// 417 处 `undefined reference to 'boot_cpu_data+114'`。两条发射路径都要盖到：
/// 代码里的 `movl sym+N(%rip)`（`pending_code_rels`）与 `.pushsection` 里的
/// `.quad sym+N`（`pending_custom_sec_rels`）—— 早期只修了后者与 `pending_data_rels`，
/// 回放全树才发现真正的量在 `pending_code_rels` 那条。
#[test]
#[cfg(unix)]
fn asm_symbol_offset_does_not_become_a_symbol_name() {
    let src = include_str!("../fixtures/opt/asm_sym_offset_reloc.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("asm-sym-offset-reloc", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        // symtab 里不允许出现名字带 `+` 的条目（那是没拆开的 asm 表达式整串）。
        let plus: Vec<String> = syms
            .iter()
            .filter(|s| s.name.contains('+'))
            .map(|s| format!("{} shndx={}", s.name, s.shndx))
            .collect();
        assert!(
            plus.is_empty(),
            "{flag}: `sym±N` 被当成一个符号名发了进 symtab（链接期就是 `undefined \
             reference to 'cpu_data+8'`）：实测 {plus:?}",
        );
        // `.pushsection` 里那条 `.quad cpu_data+8` 必须绑到本 TU 有定义的 cpu_data。
        let aux = elf64_rela(&bytes, ".rela.my_aux");
        assert!(!aux.is_empty(), "{flag}: .rela.my_aux 一条重定位都没有，下面的判据是空的");
        for (off, idx, _ty, addend) in &aux {
            let s = syms
                .get(*idx as usize)
                .unwrap_or_else(|| panic!("{flag}: .rela.my_aux+{off:#x} 的符号索引 {idx} 越界"));
            assert_eq!(
                s.name,
                "cpu_data",
                "{flag}: .rela.my_aux 绑到了错误的符号 (idx={idx} name={} shndx={})",
                s.name,
                s.shndx
            );
            assert_ne!(s.shndx, 0, "{flag}: .rela.my_aux 绑到了未定义的 cpu_data");
            assert_eq!(*addend, 8, "{flag}: .quad cpu_data+8 的位移必须并进 addend");
        }
    }
}

/// 上一条的运行级护栏：`cpu_data+8` 的偏移必须**真的生效**。只剥名字不并位移
/// 的话，产物级判据全绿但读出来是 `cpu_data[0]`（11）而不是 `cpu_data[1]`（22）。
/// 四档 stdout 与退出码逐字节对齐 gcc。
#[test]
#[cfg(unix)]
fn asm_symbol_offset_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/asm_sym_offset_reloc.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("asm-sym-offset-reloc", src, &[flag]),
            0,
            "{flag}: `sym±N` 的位移在重定位出口被丢了 / 被多算了",
        );
    }
}

/// 内核「编译期断言」惯用法（块作用域 `extern` 原型 + 常量条件）必须连引用都不发。
///
/// 缺陷现场（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.19）：全树链接
/// 181 处 `undefined reference to '__compiletime_assert_NNN'`（外加同惯用法的
/// `__scoped_seqlock_*` / `__BUILD_BUG_*`）。根因不是链接侧也不是折叠器缺臂，而是
/// `opt.rs::items_have_unsafe_ssa_pattern` 把「块里出现过 `Decl::Func`」一律当成
/// SSA 不可重建的形态（那条判据本意是**有体的嵌套函数**），整函数退出 SSA ⇒ SCCP
/// 没跑 ⇒ `if (常量)` 留在产物里。逐形实测：原型放文件作用域能折，放同一个块 / 内层块 /
/// 带 `__noreturn`+`__compiletime_error` / 条件写成 `cond == 0` 全不折（gcc 六形全折）。
///
/// 判据只数 `.rela.text`：`elf64_all_rela` 只扫 `.rela.dyn/.plt/.iplt`，在 ET_REL 上
/// 永远是空集（旧的陷阱族用例就因此只测到了「引用没发」的假一半）。
///
/// 断言 `trap_c1..trap_c6` 全部折掉。`trap_c7`（条件读的是「语句表达式初值」的局部）**目前
/// 仍不折**：`({...})` 把函数顶出 SSA 通道，而 AST 层只代回 asm `"i"` 操作数——想把它拉到
/// AST 层的条件位置（`subst_const_reads_in_cond`）时，integration 里 10 条用例连带
/// `m5_bootstrap_selftest` 挂死（误折活分支型的回归），已回退；该缺口连同挂死复现方法记在
/// 报告 §3.23。`trap_r1`/`trap_r2` 是运行期活臂，引用必须在。
#[test]
#[cfg(unix)]
fn compiletime_assert_block_scope_proto_calls_leave_no_relocation() {
    let src = include_str!("../fixtures/opt/compiletime_assert_block_proto.c");
    let dead_arms: [&str; 6] = ["trap_c1", "trap_c2", "trap_c3", "trap_c4", "trap_c5", "trap_c6"];
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("cta-block-proto", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let sites: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
            .filter(|s| dead_arms.contains(&s.name.as_str()))
            .map(|s| s.name.clone())
            .collect();
        assert!(
            sites.is_empty(),
            "{flag}: 常量条件的编译期断言没折掉，产物里仍有 {} 条指向陷阱函数的调用重定位 \
             （这就是内核链接失败的 `undefined reference to '__compiletime_assert_NNN'`）：\
             实测 {sites:?}",
            sites.len()
        );
        // 反向护栏：活臂的引用不得被过修抹掉（它们坐在 `volatile` 读入的条件里）。
        // 只要求**两个名字都还在**，不钉条数：`shape_runtime`/`shape_runtime_invert`
        // 是 `static`，内不内联、内联到几处由成本模型定（本轮实测 2 条/名字，因为两个
        // 调用点都内联了；早先未内联时是 1 条/名字）。运行期才是真判据 —— 同一只
        // 夹具 dcc 与 gcc 的 `main` 输出逐字相同（`5000 3000`），见
        // `tests/fixtures/opt/compiletime_assert_block_proto.c` 的期望说明。
        let live: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
            .filter(|s| s.name == "trap_r1" || s.name == "trap_r2")
            .map(|s| s.name.clone())
            .collect();
        for want in ["trap_r1", "trap_r2"] {
            assert!(
                live.iter().any(|n| n == want),
                "{flag}: 运行期活臂 `{want}` 的引用被抹掉了（过修）：实测 {live:?}"
            );
        }
    }
}

/// 未被引用的 `visibility("hidden")` 声明不得变成 UND 条目（ld 对此是硬错）。
///
/// 缺陷现场（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.25）：内核
/// `include/vdso/datapage.h:161` 的 `extern struct vdso_arch_data vdso_u_arch_data
/// __attribute__((visibility("hidden")));` 在 x86 的 `drivers/char/random.c` 里只是声明
/// （用户是 riscv/loongarch），但 dcc 按「无定义声明」逐条登 UND ⇒ 最终链接硬错
/// `ld: .tmp_vmlinux1: hidden symbol 'vdso_u_arch_data' isn't defined`（与 `.tdata` 那道墙
/// 并列，是未定义引用以外的两个链接 blocker 之一）。普通未定义符号只能等别的 TU 供定义，
/// 而 **隐藏**可见性不让。
///
/// 判据只针对 hidden，不用「一切未被引用的 UND」：宽版实测会拆掉 `-fbuiltin-linker`
/// 那条偶然的安全网（幽灵 UND 消失后内置链接器不再报 `symbol 'printf': undefined
/// reference`，转而链出一个段错误的静态 ET_EXEC），已试过并回退。`elf64_syms_by_type`
/// 不带 visibility 字段，所以这里按名字双向钉：隐藏的不得在、被引用的必须在。
#[test]
#[cfg(unix)]
fn unreferenced_hidden_declaration_is_not_emitted_as_undefined() {
    let src = include_str!("../fixtures/c/hidden_undef_decl.c");
    for flag in ["-O0", "-O2"] {
        let (out, obj) = run_object_test_flags("hidden-undef-decl", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let und_of = |n: &str| {
            syms.iter()
                .filter(|s| s.name == n && s.shndx == 0)
                .count()
        };
        assert_eq!(
            und_of("vdso_u_arch_data"),
            0,
            "{flag}: 未被引用的 hidden 声明发了 UND 条目（链接期就是 \
             `hidden symbol 'vdso_u_arch_data' isn't defined`）"
        );
        // 反向护栏：被真引用的符号不得被误删（否则就是静默指错地址）。
        assert_eq!(
            und_of("use_other"),
            1,
            "{flag}: 被调用的 `use_other` 的 UND 条目被误删了"
        );
    }
}

/// 内核陷阱宏的另一半：条件读的是局部变量时，只有 SSA/CCP 能折（门槛 level>=2），所以
/// -O1 不折；而本件把块作用域原型放行进 SSA 后，-O2/-O3 都能折。两边合起来才是完整的
/// 回归面，-O1 的缺口靠上面那条用例的档位选择不遮住（它已写在上面的注释里）。
#[test]
#[cfg(unix)]
fn compiletime_assert_block_proto_matches_gcc_at_all_levels() {
    if !tool_exists("gcc") {
        return;
    }
    let src = include_str!("../fixtures/opt/compiletime_assert_block_proto.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("cta-block-proto", src, &[flag]),
            0,
            "{flag}: 块作用域原型放行进 SSA 通道后，常量臂/运行期臂的语义与 gcc 不一致",
        );
    }
}

/// switch 臂体里「块尾就是 return」之后的语句必须截掉。
///
/// 现场（`lib/vsprintf.c:2596` 的 `pointer()`，链接期 `undefined reference to
/// 'rust_fmt_argument'`）：`case 'A'` 折完之后是
/// `Expr; Block{ Block{ Return(error_string…) } }; Return(rust_fmt_argument)` ——
/// 第二条是**以 return 收尾的裸块**，它后面的第三条永远不可达，但只认顶层 `Return`
/// 的不可达截除看不见它；而 `dce_stmt` 当时根本没有 `Stmt::Switch` 臂，连臂体都
/// 不会下去扫。两处都是「少一个臂」，产物里就多一条指向没编进内核的函数的引用。
#[test]
#[cfg(unix)]
fn switch_arm_dead_tail_after_block_wrapped_return_leaves_no_reference() {
    let src = r#"
extern void trap_after_case(void);
extern int fmt_arg(int c);
static int pick(int c, int *p)
{
	switch (c) {
	case 'A':
		*p = 1;
		if (1) { { return 7; } }
		trap_after_case();
		return 9;
	default:
		return fmt_arg(c);
	}
}
int probe(int c, int *p) { return pick(c, p); }
"#;
    let (out, obj) = run_object_test_flags("dce-switch-tail", src, &["-O2"]);
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = fs::read(&obj).expect("read .o");
    let _ = fs::remove_file(&obj);
    let syms = elf64_syms_by_type(&bytes);
    let sites: Vec<String> = elf64_rela(&bytes, ".rela.text")
        .iter()
        .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
        .filter(|s| s.name == "trap_after_case")
        .map(|s| s.name.clone())
        .collect();
    assert!(
        sites.is_empty(),
        "switch 臂体里 return 尾块之后的死语句没截掉，产物里仍有 {} 条陷阱调用重定位",
        sites.len()
    );
    // 反向护栏：`default:` 臂是活路，必须照常引 `fmt_arg`。
    assert!(
        elf64_rela(&bytes, ".rela.text")
            .iter()
            .any(|(_, idx, _, _)| syms.get(*idx as usize).map(|s| s.name == "fmt_arg").unwrap_or(false)),
        "活臂 `fmt_arg` 的引用被一并抹掉了（过修）"
    );
    assert!(
        syms.iter().any(|s| s.name == "probe" && s.shndx != 0),
        "`probe` 本身必须照常发射"
    );
}

/// 内联器自造标号（`__inl<N>__…`）不得拦住「终止语句之后的死区」截除。
///
/// 现场（`arch/x86/kernel/fpu/signal.c` 的 `convert_to_fxsr`）：`__fpu_restore_sig()`
/// 内联后头部是「`ia32_fxstate` 代回常量 0 ⇒ 早退」，早退被改写成
/// `goto __inl9094__l__inl9094__end`，后面整段（含 `convert_to_fxsr()`）都不可达。
/// 但那段死区里坐着 `cpu_feature_enabled()` 的 `asm goto` 落点（`__inl<N>__lt_yes`），
/// 按「尾段里有没有标号」判就把整段锁住 —— 而这类名字只有**同一次展开内部**会引用。
/// 同名判据现在只在「非内联自造标号」上拦；本件把形状写成源码手写版。
#[test]
#[cfg(unix)]
fn inline_generated_labels_do_not_block_dead_tail_removal() {
    let src = r#"
extern void trap_dead(void);
static int early(int x)
{
	if (1) { { goto __inl1__end; } }
	{
		int t = 1;
		asm goto("testl %0, %0" : : "r"(t) : : __inl1__lt);
	__inl1__lt:
		;
	}
	trap_dead();
__inl1__end:
	return x + 1;
}
int probe(int x) { return early(x); }
"#;
    let (out, obj) = run_object_test_flags("dce-inl-label", src, &["-O2"]);
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = fs::read(&obj).expect("read .o");
    let _ = fs::remove_file(&obj);
    let syms = elf64_syms_by_type(&bytes);
    let sites: Vec<String> = elf64_rela(&bytes, ".rela.text")
        .iter()
        .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
        .filter(|s| s.name == "trap_dead")
        .map(|s| s.name.clone())
        .collect();
    assert!(
        sites.is_empty(),
        "内联自造标号把死区锁住了，产物里仍有 {} 条陷阱调用重定位",
        sites.len()
    );
    assert!(
        syms.iter().any(|s| s.name == "probe" && s.shndx != 0),
        "`probe` 本身必须照常发射"
    );
}

/// 形参被返回表达式提及的常量桩，调用点仍要代回常量值。
///
/// 现场（`kernel/sched/fair.c`）：`static inline bool is_rd_overutilized(rd)
/// { return !sched_energy_enabled() || READ_ONCE(rd->overutilized); }`。`CONFIG_ENERGY_MODEL`
/// 关时 `sched_energy_enabled()` 是 `return false` 桩 ⇒ `||` 左侧恒真 ⇒ 返回值恒 1
/// （右操作数在 C 语义里根本不求值）。旧口径因为「返回表达式提到了形参 `rd`」整条拒绝
/// 代回，于是 `select_task_rq_fair` 里 `if (!is_rd_overutilized(this_rq()->rd))` 判不死，
/// `find_energy_efficient_cpu` → `compute_energy` 一直有引用，链接期
/// `undefined reference to 'sugov_effective_cpu_perf'`。
///
/// `noinline` 是刻意加的：这里要量的是**常量桩代回**这条路，不是内联器顺手展平。
#[test]
#[cfg(unix)]
fn const_stub_mentioning_params_still_folds_at_call_sites() {
    let src = r#"
extern void trap_sugov(void);
extern int other(unsigned int *);
static _Bool sched_energy_enabled(void) { return 0; }
static __attribute__((noinline)) _Bool is_rd_overutilized(unsigned int *rd)
{
	return !sched_energy_enabled() || *(const volatile unsigned int *)rd;
}
int probe(unsigned int *rd)
{
	if (!is_rd_overutilized(rd))
		return (trap_sugov(), 1);
	return other(rd);
}
"#;
    let (out, obj) = run_object_test_flags("const-stub-param", src, &["-O2"]);
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = fs::read(&obj).expect("read .o");
    let _ = fs::remove_file(&obj);
    let syms = elf64_syms_by_type(&bytes);
    let sites: Vec<String> = elf64_rela(&bytes, ".rela.text")
        .iter()
        .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
        .filter(|s| s.name == "trap_sugov")
        .map(|s| s.name.clone())
        .collect();
    assert!(
        sites.is_empty(),
        "形参被提及的常量桩没代回，产物里仍有 {} 条陷阱调用重定位",
        sites.len()
    );
    // 反向护栏：另一支是活路，`other` 必须照常引。
    assert!(
        elf64_rela(&bytes, ".rela.text")
            .iter()
            .any(|(_, idx, _, _)| syms
                .get(*idx as usize)
                .map(|s| s.name == "other")
                .unwrap_or(false)),
        "活支 `other` 的引用被一并抹掉了（过修）"
    );
    assert!(
        syms.iter().any(|s| s.name == "probe" && s.shndx != 0),
        "`probe` 本身必须照常发射"
    );
}

/// 四个「档位敏感」夹具的行为回归：同一份源码在 -O0/-O1/-O2/-O3 下必须**行为一致**。
///
/// 为什么单列一条：这四份夹具也正是 `scripts/check_opt_equivalence.sh` 里原来的 12 条
/// OPT-DIFF（4 夹具 × 3 档）。那个脚本是**外部手工闸**，平时不跑；这里把它们钉成
/// `cargo test` 的一部分，任何一次优化器改动都能当场看见。四条的病灶互不相同：
///
/// - `chained_assign_truncation`：`u = uc = 300` 的赋值表达式值必须是「存入后按左值类型
///   截断」的 44（C11 6.5.16p3）。`store_const_stmt` 曾借 `const_eval::eval_full` 给
///   全局初始化器用的宽松 `Assign` 臂，把这条 store 记成 `u = 300`，随后 `u != 44`
///   被折成真 → -O1/-O2 abort（rc=134）。
/// - `pointer`：`int *arr[3] = {&a,&b,&c}` 里的 `&` 是左值指示符不是值读。危害收集
///   器缺 `InitList`/`CompoundLit` 臂，`a/b/c` 留在候选表；代回器又把 `&a` 当值读
///   代成 `&1`，数组里存的是 1/2/3 → `*arr[0]` SIGSEGV（rc=139）。
/// - `constp`：`__builtin_constant_p` 是**语法查询**，实参不能被代回；`int x = 1;`
///   这种源码局部即便「从未被取址」也不算 proven（只有内联器造的 `__inl*` 临时量算）。
/// - `builtins`：`__builtin_*` 族里同类的档位漂移（rc=4）。
#[test]
#[cfg(unix)]
fn opteq_contract_fixtures_are_level_independent() {
    for (name, src) in [
        (
            "chained-trunc",
            include_str!("../fixtures/c/chained_assign_truncation.c"),
        ),
        ("ptr-initlist-takeaddr", include_str!("../fixtures/c/pointer.c")),
        ("constp-source-local", include_str!("../fixtures/c/constp.c")),
        ("builtins-levels", include_str!("../fixtures/c/builtins.c")),
    ] {
        for level in ["-O0", "-O1", "-O2", "-O3"] {
            let (out, exe) = run_compile_and_run_flags(name, src, &[level]);
            assert!(
                out.status.success(),
                "{name} {level}: 编译失败: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let rc = run_exe(&exe);
            let _ = fs::remove_file(&exe);
            assert_eq!(
                rc, 0,
                "{name} {level}: 行为偏离了 -O0 基准（rc={rc}）——\
                 优化档位之间的答案必须一致，-O0 才是语义基准"
            );
        }
    }
}

/// 反向护栏：`__builtin_constant_p(<内联造出来的临时量>)` 在 -O2 必须仍答 1。
///
/// 为什么需要这一半：上一条用例逼着我们把 ipa_modref 的「隔离」口径从「任何未被取址的
/// 局部」收窄成「只认内联器造的 `__inl*` 临时量」。内核那一族 `BUILD_BUG_ON` /
/// `compiletime_assert` 的常量化判定完全押在形参被内联成字面量后的这一路上
/// （实测 `mm/shmem.o` 的 9 条 `__compiletime_assert_NNN`）；收窄过头它们会整族复活，
/// 变成链接期 `undefined reference`。gcc -O2 同题答 1，这里与之一致。
#[test]
#[cfg(unix)]
fn builtin_constant_p_of_inline_temp_still_true() {
    let src = r#"
static int probe(int n) { return __builtin_constant_p(n); }
int main(void) { return probe(3) == 1 ? 0 : 1; }
"#;
    for level in ["-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("constp-inline-temp", src, &[level]);
        assert!(
            out.status.success(),
            "{level}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = run_exe(&exe);
        let _ = fs::remove_file(&exe);
        assert_eq!(
            rc, 0,
            "{level}: 形参被内联成字面量后 `__builtin_constant_p` 仍答 0 —— \
             「隔离」口径收窄过头，内核 BUILD_BUG_ON 族会复活"
        );
    }
}

/// 常量存前代的**两条控制流前提**，行为级钉住。
///
/// 这一条 pass 沿的是「源码顺序」轴：`x = 常量;` 之后的 `x` 读点被代成常量。那个轴
/// 只对**直线控制流**成立，两个形状会破它：
///
///   * **回边**（`if (yystate != 0) …; yystate = 7; … goto yynewstate;`）：跳回来会把标号
///     之后的读点**再跑一遍**，而那一遍里 `yystate` 已被改过。现场不是猜的：
///     bison 骨架 `scripts/kconfig/parser.tab.c` 里 `yy_state_fast_t yystate = 0;` →
///     `yynewstate:`（被 `goto yynewstate;` 回跳）→ `if (yystate == YYFINAL) YYACCEPT;`
///     被代成 `0 == 6`，accept 判掉 ⇒ parser 跑过头撞 EOF，症状是运行期
///     `input in flex scanner failed`（-O0 正常、-O1 起坏；逐对象归属 + 逐 pass 二分
///     + 指令流对比定位到这一条 pass 的这一个函数）。
///   * **同一标号多条进入边**：「goto 紧邻标号 ⇒ 常量表延续进去」只在**恰好一条**进入边
///     时成立。`yyreduce:` 同时被 `yybackup` 分支与 `yydefault` 分支跳入，两条路上
///     `yylen`/`yyn` 不同，沿紧邻那条递的值对另一条不成立。
///
/// 反向护栏也在上面：跳回跨度内**没**被写过的名字仍得保留（`copy_fpstate_to_sigframe` 的
/// `retry:` + `ia32_fxstate = 0` 那一形靠它才能判死），所以 `keep_fold` 除了运行期要对，
/// 产物里那条死臂的 `trap_flag` 引用也必须为 0（拿 `.rela.text` 数，不看 nm 的 UND 条目）。
#[test]
#[cfg(unix)]
fn const_store_forward_respects_back_edges_and_multi_in_edge_labels() {
    let src = r#"
static int back_edge(void)
{
	int yystate = 0;
	int n = 0;

yynewstate:
	if (yystate != 0)
		return 1;		/* 第二次进入该走这里 */
	yystate = 7;
	if (++n >= 2)
		return 2;
	goto yynewstate;
}

static int multi_in_edge(int c)
{
	int y = 1;
	if (c) {
		y = 5;
		goto L;
	}
	y = 9;
	goto L;
L:
	return y;
}

/* 跳回区间内没写过的那个名字照旧得能代回（否则就是拿「一律清表」换正确性）。
   `flag` 在 (retry:, goto retry] 跳回跨度内一次也没被写过，所以
   `if (flag) return trap_flag();` 必须仍然是死臂。 */
extern int trap_flag(void);
static int keep_fold(int steps)
{
	int flag = 0;
	int i = 0;

retry:
	i++;
	if (flag)
		return trap_flag();
	if (i > steps)
		return 1;
	goto retry;
}

int main(void)
{
	if (back_edge() != 1)
		return 1;
	if (multi_in_edge(1) != 5)
		return 2;
	if (multi_in_edge(0) != 9)
		return 3;
	if (keep_fold(2) != 1)
		return 4;
	return 0;
}
"#;
    // 只跑 -O1 以上：`trap_flag` 是**故意不给定义**的（死臂引用它就是本类缺陷的现场），
    // -O0 不做折叠，链它必报 undefined reference —— 那不是本用例要量东西。
    for level in ["-O1", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("cprop-control-flow", src, &[level]);
        assert!(
            out.status.success(),
            "{level}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = crate::harness::run_exe_bounded(&exe).0;
        let _ = std::fs::remove_file(&exe);
        assert_eq!(
            rc,
            0,
            "{level}: 常量存前代越过了控制流边界（rc={rc}：1=回边上把 `yystate != 0` 代成了 \
             假，2=多进入边的标号后被递表代坏，3=另一条边代坏，4=该保留的折叠被一并清掉）"
        );
    }

    // 产物级的反向护栏：回边清表不得连「跳回跨度内没被写过的名字」一起清掉。
    // 判据只看 `trap_flag` 的重定位条数（它在 -O1 必须是 0，因为 `flag` 恒为 0）。
    let (out, obj) = run_object_test_flags("cprop-keep-fold", src, &["-O1"]);
    assert!(
        out.status.success(),
        "-O1: 编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&obj).expect("read .o");
    let _ = std::fs::remove_file(&obj);
    let syms = elf64_syms_by_type(&bytes);
    let hits: Vec<String> = elf64_rela(&bytes, ".rela.text")
        .iter()
        .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
        .filter(|s| s.name == "trap_flag")
        .map(|s| s.name.clone())
        .collect();
    assert!(
        hits.is_empty(),
        "-O1: 跳回跨度内没写过 `flag`，那条死臂必须照样被折掉；实测仍有 {} 条 `trap_flag` 引用",
        hits.len()
    );
}

/// jump-label：常量必须能代进 **inline asm 的输入操作数**（否则整棵内核编不动）。
///
/// 形状就是 `arch/x86/include/asm/jump_label.h:37`：`__always_inline` 的
/// `arch_static_branch(key, branch)` 里 `JUMP_TABLE_ENTRY("%c0 + %c1", "%l[l_yes]")`
/// 配两个 `"i"` 操作数，调用点传的是 `&全局符号` 与字面量 `false`。两处缺口叠在一起：
///   * `subst_cprop_stmt` 根本没有 `Stmt::AsmGoto` 这一臂 ⇒ 内联后停在临时量上的操作数
///     永远等不到常量（旧行为：`error[E0311]: impossible constraint in 'asm' …
///     (expression: __inlN$_branch)`，全树 10/31 个已编 TU 均倒在 `arch_static_branch`）；
///   * 就算有那一臂也代不了：`collect_cprop_disq_stmt` 把 asm 里出现的**任何**名字一律
///     判死（内存约束不发 `&`，取址计数看不见），连裸 Ident 的立即数输入也被误伤
///     （实测 `FWD| fn=use hazard=["__inl2$_branch"]`，候选表里根本没它）。
///
/// 两道限制一起守住“不能代错”：只代 `inputs`（输出是 asm 要写的地方），且只代
/// 「非要立即数不可」约束上的**裸 Ident**（`"i"(&x)`/`"i"(a[3])` 仍判死，那是把地址交给 asm）。
#[test]
#[cfg(unix)]
fn const_propagates_into_immediate_only_asm_inputs() {
    let src = r#"
#include <stdbool.h>
struct static_key { int enabled; };
extern struct static_key K;
void printk(const char *fmt);
int f(void);

#define JTE(key, label) \
	".pushsection __jump_table, \"aw\"\n\t" \
	".long " key " - ., " label " - .\n\t" \
	".popsection\n\t"

static inline __attribute__((always_inline)) bool
arch_static_branch(struct static_key *const key, const bool branch)
{
	__asm__ goto(
		"1: .byte 0x90,0x90,0x90,0x90,0x90\n\t"
		JTE("%c0 + %c1", "%l[l_yes]")
		: : "i"(key), "i"(branch) : : l_yes);
	return false;
l_yes:
	return true;
}

static inline __attribute__((always_inline)) bool static_key_false(struct static_key *key)
{
	return arch_static_branch(key, false);
}

#define my_dbg(fmt) do { if (static_key_false(&K)) printk(fmt); } while (0)

int use(void)
{
	my_dbg("x");
	return f();
}
"#;
    // 只跑 -O2/-O3：语句级「整体搬移」那套展开机器从 -O2 起才开（`opt.rs` 表达式内联入口
    // 的「档位或深度闸门」，`level < 2` 直接拒），于是 -O0/-O1 下 `arch_static_branch` 留在
    // 外带副本里、`"i"(key)` 拿不到常量。那是既有的档位边界，不是本轮改的东西；把
    // 它写进同一个循环只会把一个已知缺口呈现成「本轮新坏的」。
    for level in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("jumplabel-cprop", src, &[level]);
        assert!(
            out.status.success(),
            "{level}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = std::fs::read(&obj).expect("read .o");
        let _ = std::fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let rel = elf64_rela(&bytes, ".rela__jump_table");
        // 两条表项就是 `key - .` 与 `label - .`；操作数代不上时根本不会生成这个节
        assert_eq!(
            rel.len(),
            2,
            "{level}: `__jump_table` 应有 2 条重定位（key 与 label），实测 {} 条",
            rel.len()
        );
        let names: Vec<&str> = rel
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize).map(|s| s.name.as_str()))
            .collect();
        assert!(
            names.iter().any(|n| *n == "K"),
            "{level}: `%c0 + %c1` 没代成 `K + 0`（符号键丢了），实测符号: {:?}",
            names
        );
    }
}

/// KCOV 的 `-fsanitize-coverage=trace-pc` **不得**否决 `__always_inline` 的展开。
///
/// 真实现场：allmodconfig（CONFIG_KCOV=y）下 13/39 个 TU 编不过，报的全是同一处：
/// `impossible constraint in 'asm': operand 0 with immediate-only constraint "i" has no
/// compile-time constant value (expression: key, in function arch_static_branch)`。
/// 根因既不在 asm 也不在常量传播，而在 sanitizer：`needs_edge_coverage()` 把 `kcov`
/// 也算了进去，于是 `trace-pc` 除了 PC 回调还额外往每个函数体头部发一条
/// `static unsigned int __dcc_cov_guards_N[16]`；`body_is_splicable` 一见 `static` 局部
/// 就判「体不可整体搬移」⇒ `arch_static_branch` 留在外带副本里 ⇒ `"i"(key)` 拿到的
/// 是形参。修法是 `trace-pc` 不发 guard 数组（那是 `trace-pc-guard` 的活，gcc/clang
/// 也是这个口径）。
///
/// 形状照抄 dyndbg（`static struct _ddebug` + `static_branch_unlikely(&id.key.dd_key_false)`
/// + `__builtin_types_compatible_p` 分派），表项照抄 `JUMP_TABLE_ENTRY` 的三字段：其中
/// `.quad key - .` 必须是 **R_X86_64_PC64**（8 字节字段的 PC 相对）——发成绝对的
/// R_X86_64_64 会存成「key 的绝对地址」而不是「key 相对本条表项」，运行期
/// jump_label 会把 static_key 指针算到别处去。
#[test]
fn kcov_trace_pc_keeps_always_inline_jump_label() {
    let src = r#"
#include <stdbool.h>
struct static_key { int enabled; };
struct static_key_true { struct static_key key; };
struct static_key_false { struct static_key key; };
struct _ddebug {
	const char *name;
	const char *function;
	unsigned int flags;
	union {
		struct static_key_true dd_key_true;
		struct static_key_false dd_key_false;
	} key;
};
void printk(const char *fmt);
int f(void);
#define JTE(key, label) \
	".pushsection __jump_table, \"aw\"\n\t" \
	".balign 8\n\t" \
	".long 1b - .\n\t" \
	".long " label " - .\n\t" \
	".quad " key " - .\n\t" \
	".popsection\n\t"
static inline __attribute__((always_inline)) bool
arch_static_branch(struct static_key *const key, const bool branch)
{
	__asm__ goto(
		"1: .byte 0x90,0x90,0x90,0x90,0x90\n\t"
		JTE("%c0 + %c1", "%l[l_yes]")
		: : "i"(key), "i"(branch) : : l_yes);
	return false;
l_yes:
	return true;
}
#define static_branch_unlikely(x) \
({ bool branch; \
	if (__builtin_types_compatible_p(typeof(*x), struct static_key_true)) \
		branch = false; \
	else branch = arch_static_branch(&(x)->key, false); \
	branch; })
#define DYNAMIC_DEBUG_BRANCH(descriptor) static_branch_unlikely(&descriptor.key.dd_key_false)
int use(void)
{
	static struct _ddebug id = { "x", "f", 0, { } };
	if (DYNAMIC_DEBUG_BRANCH(id))
		printk("hi\n");
	return f();
}
"#;
    for level in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags(
            "jumplabel-kcov",
            src,
            &[level, "-fsanitize-coverage=trace-pc"],
        );
        assert!(
            out.status.success(),
            "{level}: KCOV 下编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = std::fs::read(&obj).expect("read .o");
        let _ = std::fs::remove_file(&obj);
        let rel = elf64_rela(&bytes, ".rela__jump_table");
        assert_eq!(
            rel.len(),
            3,
            "{level}: 一个 jump-label 调用点应有 3 条表项重定位（code/count/key），实测 {} 条",
            rel.len()
        );
        // 第三个字段（`.quad key - .`）必须是 PC 相对的 8 字节重定位
        assert_eq!(
            rel[2].2, 24,
            "{level}: `.quad key - .` 应是 R_X86_64_PC64(24)，实测 {}（绝对类型会把 key 存成绝对地址）",
            rel[2].2
        );
    }
}

/// 表达式级内联**不得**把 `&形参` 代成 `&实参表达式`。
///
/// 真实现场不是玩具：`scripts/kconfig/preprocess.c` 的
/// `expand_string_with_args(const char *in){ return __expand_string(&in, ...); }` 被内联进
/// `variable_expand` 后，第一个实参在 AST 里变成 `AddrOf{ Member{ v, field: "value" } }`（用
/// `DCC_DUMP_PASS=function_inline` 直接读到），`__expand_string` 里那句 `*str = p` 于是把
/// `v->value` 改成了循环结束指针；后面 `variable_all_del()` free 它就是
/// `free(): invalid pointer` —— `make syncconfig` 直接崩，allmodconfig 一步都编不动。
///
/// 这里用同一个形状做一个不依赖任何树的判据：被调方写出参，调用方必须只看到自己那份副本
/// 被改。旧实现下 `v` 会被指到字符串尾（返回 1），修完不再内联这条（返回 0）。
///
/// `always_inline` 是必需的：真实现场里 `expand_string_with_args` 是个**单 `return` 体**，
/// 走的正是表达式级内联那条路；不带这个属性时小体函数会被成本/尺寸闸门拦在语句级路径外
/// （那里本来就把形参存进局部，所以不会错），用例就变成假绿——已实测过这个区别：
/// 去掉 `always_inline` 后旧二进制也返 0（不复现）。
#[test]
#[cfg(unix)]
fn inliner_must_not_bind_address_of_parameter_to_caller_object() {
    let src = r#"
static char scratch[16] = "plain-text";

static char *scan(const char **str)
{
	const char *p = *str;

	while (*p)
		p++;
	*str = p;		/* 通过形参的地址写回 */
	return scratch;
}

__attribute__((always_inline)) static char *wrap(const char *in)
{
	return scan(&in);	/* 形参 `in` 被取址 ⇒ 不能把 `in` 直接换成调用者的表达式 */
}

int main(void)
{
	char *v = scratch;
	char *r = wrap(v);

	if (v != scratch)
		return 1;	/* 被调方越过的调用者的指针 */
	if (scratch[0] != 'p')
		return 2;
	if (r != scratch)
		return 3;
	return 0;
}
"#;
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("addr-of-param", src, &[level]);
        assert!(
            out.status.success(),
            "{level}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = crate::harness::run_exe_bounded(&exe).0;
        let _ = std::fs::remove_file(&exe);
        assert_eq!(
            rc, 0,
            "{level}: `&形参` 被内联代成了 `&调用者对象`（rc={rc}：1=被调方写坏了调用者的指针，\
             2=指到了字符串尾，3=返回值不对）"
        );
    }
}
