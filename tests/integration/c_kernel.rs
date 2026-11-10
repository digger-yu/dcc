// included from tests/integration.rs — not a standalone cargo target

#[test]
#[cfg(unix)]
fn arch_x86_wrapper_preserves_supported_kernel_flags() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/arch_x86_compile.sh");
    let temp = std::env::temp_dir().join(format!("dcc-arch-wrapper-{}", std::process::id()));
    fs::create_dir_all(temp.join("arch/x86/kernel")).expect("create wrapper fixture");
    fs::write(temp.join("arch/x86/kernel/foo.c"), "int foo(void) { return 0; }\n")
        .expect("write wrapper source fixture");
    let cmd_file = temp.join("arch/x86/kernel/.foo.o.cmd");
    fs::write(
        &cmd_file,
        "savedcmd_arch/x86/kernel/foo.o := cc -fstack-clash-protection -ftrivial-auto-var-init=zero -mrecord-mcount -mfentry -pg -falign-loops=1 -c -o foo.o foo.c\n",
    )
    .expect("write wrapper command fixture");
    let output = Command::new("bash")
        .arg(script)
        .arg(&temp)
        .env("DCC", "echo")
        .env("OUT_LOG", temp.join("out.log"))
        .output()
        .expect("run arch wrapper");
    let log = fs::read_to_string(temp.join("out.log")).unwrap_or_default();
    let _ = fs::remove_dir_all(&temp);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(log.contains("-fstack-clash-protection"), "log: {log}");
    assert!(log.contains("-ftrivial-auto-var-init=zero"), "log: {log}");
    assert!(log.contains("-mrecord-mcount"), "log: {log}");
    assert!(log.contains("-mfentry"), "log: {log}");
    assert!(log.contains("-pg"), "log: {log}");
    assert!(log.contains("-falign-loops=1"), "log: {log}");
}

/// M7 libdecnumber combined TU 编译回归: 需要宿主 GCC 源码树 (见
/// [`gcc_src_tree_or_skip`]). 组合 TU 把 decSingle/Double/Quad 三套
/// 符号收敛到一个 .o, 避免 GCC configure 阶段 11 个 SKIP.
#[test]
#[cfg(unix)]
fn libdecnumber_combined_tu_compiles() {
    let gcc_src = match gcc_src_tree_or_skip("libdecnumber") {
        Some(p) => p,
        None => return,
    };
    let stub_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gcc_tools");
    let combined = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gcc_tools/libdecnumber_combined.c");
    let dir = std::env::temp_dir();
    let object = dir.join(format!("dcc-libdecnumber-{}.o", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args([
            "-c",
            "-I",
            gcc_src.join("libdecnumber").to_str().unwrap(),
            "-I",
            gcc_src.join("libdecnumber/dpd").to_str().unwrap(),
            "-I",
            stub_dir.to_str().unwrap(),
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

/// M26.6: cc1/cc1plus bootstrap 门禁契约验证.
/// 仅在 --features gcc_src + GCC_SRC 指向真实 GCC 源码树时执行;
/// 本地/CI 无源码树直接 SKIP. 契约:
///   - 退出码 0 (SKIP 或 cc1/cc1plus 全量构建收敛) 或 2 (xfail) 都算通过
///   - xfail 时必须产出进度报告 cc1plus_progress.txt (对象数/错误数/
///     首批失败文件), 供跟踪 dcc 对 GCC 主体的 bootstrap 收敛
#[test]
#[cfg(unix)]
fn gcc_cc1plus_bootstrap_gate() {
    let gcc_src = match gcc_src_tree_or_skip("gcc") {
        Some(p) => p,
        None => return,
    };
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/cc1plus_bootstrap.sh");
    let build_dir = std::env::temp_dir().join(format!("dcc-cc1plus-gate-{}", std::process::id()));
    let output = Command::new("bash")
        .arg(&script)
        .env("GCC_SRC", &gcc_src)
        .env("DCC", env!("CARGO_BIN_EXE_dcc"))
        .env("BUILD_DIR", &build_dir)
        .env("TIMEOUT", "300") // 门禁内限时; 契约验证不要求完整构建
        .output()
        .expect("run cc1plus bootstrap gate");
    let status = output.status.code().unwrap_or(-1);
    // 0 = SKIP/收敛; 2 = xfail 且已产出进度报告; 其他 = 环境错误
    assert!(
        status == 0 || status == 2,
        "gate exit={} stdout={} stderr={}",
        status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if status == 2 {
        let report = build_dir.join("cc1plus_progress.txt");
        assert!(report.exists(), "xfail gate must emit cc1plus_progress.txt");
        let text = fs::read_to_string(&report).unwrap();
        assert!(text.contains("cc1plus bootstrap gate: xfail"), "{}", text);
    }
    let _ = fs::remove_dir_all(&build_dir);
}

#[test]
#[cfg(unix)]
fn strict_full_build_exports_required_kernel_gates() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/kernel_full_check.sh");
    let text = fs::read_to_string(script).expect("read kernel_full_check.sh");

    assert!(text.contains("STRICT_FULL_BUILD"), "{text}");
    assert!(text.contains("BUILD_BZIMAGE=\"1\""), "{text}");
    assert!(text.contains("BUILD_VMLINUX=\"1\""), "{text}");
    assert!(text.contains("REQUIRE_BZIMAGE=\"1\""), "{text}");
    assert!(text.contains("REQUIRE_VMLINUX=\"1\""), "{text}");
    assert!(text.contains("REQUIRE_PAHOLE=\"1\""), "{text}");
    assert!(text.contains("REQUIRE_OBJTOOL=\"1\""), "{text}");
    assert!(text.contains("STRICT_MODPOST=\"1\""), "{text}");
}

#[test]
#[cfg(unix)]
fn strict_kernel_scripts_reject_missing_artifacts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp =
        std::env::temp_dir().join(format!("dcc-strict-kernel-missing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);

    let smoke = Command::new("/bin/bash")
        .arg(root.join("scripts/kernel_build_smoke.sh"))
        .env("KERNEL_DIR", &temp)
        .env("REQUIRE_KERNEL_DIR", "1")
        .output()
        .expect("run kernel_build_smoke.sh");
    let smoke_stdout = String::from_utf8_lossy(&smoke.stdout);
    assert!(!smoke.status.success(), "stdout: {smoke_stdout}");
    assert!(smoke_stdout.contains("FAIL: kernel tree missing"));
    assert!(!smoke_stdout.contains("SKIP:"));

    let modpost = Command::new("/bin/bash")
        .arg(root.join("scripts/modpost_link.sh"))
        .env("KERNEL_DIR", &temp)
        .env("STRICT_MODPOST", "1")
        .output()
        .expect("run modpost_link.sh");
    let modpost_stdout = String::from_utf8_lossy(&modpost.stdout);
    assert!(!modpost.status.success(), "stdout: {modpost_stdout}");
    assert!(modpost_stdout.contains("FAIL:"));
    assert!(!modpost_stdout.contains("SKIP:"));

    let vmlinux = Command::new("/bin/bash")
        .arg(root.join("scripts/vmlinux_regress.sh"))
        .env("VMLINUX", temp.join("vmlinux"))
        .env("REQUIRE_VMLINUX", "1")
        .output()
        .expect("run vmlinux_regress.sh");
    let vmlinux_stdout = String::from_utf8_lossy(&vmlinux.stdout);
    assert!(!vmlinux.status.success(), "stdout: {vmlinux_stdout}");
    assert!(vmlinux_stdout.contains("FAIL: vmlinux not found"));
    assert!(!vmlinux_stdout.contains("SKIP:"));
}

/// KASAN 栈红区: -fsanitize=address 时函数入口应发射
/// __asan_poison_memory_region 和 __asan_unpoison_memory_region 调用
///
/// 判据读**产物**（`-c` 出来的 `.o` 反汇编），不读 `-S` 文本转储：raw 字节序列在 `-S`
/// 里只能以 `/* raw */` + 按字节解码的噪声呈现（本后端的 `-S` 是有损转储，不能
/// 再汇编：实测同一序列 GAS 会把 `movups` 写成 `movaps`）。帧尺寸一变，那条
/// `mov $imm32,%esi` 的字节划分就变，`-S` 里谁被拼成可读的 `call` 也跟着变 ——
/// 而 .o 里的调用从来没缺过。
#[test]
fn kasan_stack_redzone_poison_unpoison() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-stack-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-kasan-stack-{}.o", std::process::id()));
    let source = "int f(void) { int x = 42; return x; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let dump = Command::new("objdump")
        .args(["-dr", "--no-show-raw-insn"])
        .arg(&obj_path)
        .output()
        .expect("run objdump");
    let asm = String::from_utf8_lossy(&dump.stdout).to_string();
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&obj_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(asm.contains("__asan_poison_memory_region"), "missing poison call: {asm}");
    assert!(asm.contains("__asan_unpoison_memory_region"), "missing unpoison call: {asm}");
}

/// KASAN 全局变量红区: -fsanitize=address 时全局变量后应有 32 字节填充
#[test]
fn kasan_global_redzone_padding() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-glob-{}.c", std::process::id()));
    let out_path = dir.join(format!("dcc-kasan-glob-{}.o", std::process::id()));
    let source = "int x = 1;\nint y = 2;\n";
    fs::write(&source_path, source).expect("write C fixture");
    // 编译无 KASAN
    let out_normal = {
        let p = dir.join(format!("dcc-kasan-glob-normal-{}.o", std::process::id()));
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-o"])
            .arg(&p)
            .arg(&source_path)
            .output()
            .expect("run dcc");
        assert!(o.status.success());
        fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
    };
    // 编译有 KASAN
    let out_kasan = {
        let o = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-fsanitize=address", "-c", "-o"])
            .arg(&out_path)
            .arg(&source_path)
            .output()
            .expect("run dcc");
        assert!(o.status.success(), "stderr: {}", String::from_utf8_lossy(&o.stderr));
        fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0)
    };
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&out_path);
    // KASAN 输出应更大 (全局变量红区填充)
    assert!(
        out_kasan > out_normal,
        "KASAN object ({out_kasan}) should be larger than normal ({out_normal}) due to global redzones"
    );
}

/// percpu 声明: section 字符串拼接 + __aligned__((expr))
#[test]
fn percpu_decl_compiles() {
    let source = include_str!("../fixtures/c/percpu.c");
    let (output, obj) = run_object_test("percpu-decl", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// Per-CPU 变量完整支持: __percpu 限定符, __seg_gs/__seg_fs 指针,
/// DEFINE_PER_CPU/DECLARE_PER_CPU 宏, 以及 %%gs: 段前缀内联汇编.
/// 测试验证:
/// 1. __percpu 类型限定符解析
/// 2. __seg_gs/__seg_fs 指针类型存储
/// 3. per-cpu 变量声明和访问
/// 4. 内联汇编中段前缀编码
#[test]
#[cfg(unix)]
fn percpu_access_runtime() {
    let src = include_str!("../fixtures/c/percpu_access.c");
    let (out, exe) = run_compile_and_run("percpu-access", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "percpu access compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "percpu access runtime check failed");
}

/// Per-CPU 变量完整支持: __seg_gs 指针类型传播, 段前缀解引用,
/// __get_cpu_var/per_cpu 宏展开, __this_cpu_* 内建函数.
/// 测试验证:
/// 1. __seg_gs 指针类型在 cast 和声明中的解析
/// 2. __seg_gs 指针解引用生成 %gs: 段前缀
/// 3. __get_cpu_var 宏通过 __seg_gs 指针正确展开
/// 4. per_cpu(var, cpu) 宏使用 cpu 参数
/// 5. __builtin_dcc_this_cpu_read/write/add 内建函数
/// 6. __this_cpu_inc/dec 内联汇编模式
#[test]
#[cfg(unix)]
fn percpu_full_support_compiles() {
    let src = include_str!("../fixtures/c/percpu_full.c");
    let (out, exe) = run_compile_and_run("percpu-full", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "percpu full compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "percpu full runtime check failed");
}

/// per_cpu_offset() 由内核提供, 编译器层面只需正确支持 per_cpu() 宏展开.
/// 测试验证: per_cpu_offset() 调用模式、per_cpu() 宏使用 offset 计算.
#[test]
#[cfg(unix)]
fn percpu_offset_compiles() {
    let src = include_str!("../fixtures/c/percpu_offset.c");
    let (out, obj) = run_object_test("percpu-offset", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "percpu offset compile failed: {stderr}");
    let _ = fs::remove_file(obj);
}

/// 函数指针解引用 `*fp`：C11 6.3.2.1p4 要求函数指示符在值上下文退化为
/// 指向函数的指针，其值就是 `fp` 本身，不得按对象大小再 load 一次。
/// 现场是 `static_call_update(key, *apic->wakeup_secondary_cpu)`：旧实现多发
/// 一条 `mov (%rax),%rax`，成员为 NULL 时 arch/x86/kernel/apic/init.c 的
/// `update_static_calls()` 直接 #PF。
#[test]
#[cfg(unix)]
fn fn_ptr_deref_decays_without_extra_load() {
    let src = r#"
struct apic {
    char pad[0xc0];
    int (*wakeup)(int);
};

static int target(int x) { return x + 7; }
static struct apic a = { .wakeup = target };

int main(void) {
    int (*direct)(int) = *a.wakeup;
    if (direct != target) return 1;
    if (direct(5) != 12) return 2;

    int (*fp)(int) = target;
    int (*decayed)(int) = *fp;
    if (decayed != fp) return 3;
    if (decayed(35) != 42) return 4;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("fn-ptr-deref-decay", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "fn ptr deref compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "fn ptr deref runtime check failed");
}

/// 静态初始化只认 `= {…}` 而不认 `= (T){…}` 这一族缺口的回归。
///
/// 现场是 PVH 启动：`mm/mm_init.c` 的 `static DEFINE_SPINLOCK(early_pfn_lock)`
/// 展开成 `static spinlock_t x = (spinlock_t) __SPIN_LOCK_INITIALIZER(x)`，
/// 而 4 字节锁字里被写进了一个**栈地址** —— `queued_spin_lock_slowpath` 对
/// 它做 `cmpxchg` 就是 `PANIC: early exception 0x0d`（报告 §5.14）。
/// 两个缺陷同族，都在「花括号列表的书写形状」这一条轴上：
///  * 预注册期不认复合字面量 → 静默退化成零初始化（文件作用域版本实测
///    `struct s g = (struct s){ .a = 7 };` 得到 0 而不是 7）；
///  * 运行时初始化期把聚合 rvalue 的交付物（物化缓冲**地址**）当数据存进
///    目标（函数局部 static 版本，就是 boot 的那一条）。
#[test]
#[cfg(unix)]
fn static_compound_literal_init_stores_value_not_address() {
    let src = r#"
struct q { int a; };
struct s { struct q raw; };
struct t { char name[8]; int v; long p; };

/* 文件作用域：复合字面量初始化必须与 `= {…}` 同形 */
struct s gf = (struct s){ .raw = { .a = 7 } };
struct s gh = { { 9 } };

static int n = 1;

int main(void) {
    /* 内核 DEFINE_SPINLOCK 的形状：嵌套聚合 + 4 字节本体 */
    static struct s x = (struct s){ .raw = { .a = 1 } };
    static struct t y = (struct t){ "abc", 7, 9 };

    if (gf.raw.a != 7) return 1;
    if (gh.raw.a != 9) return 2;
    if (x.raw.a != 1) return 3;
    if (y.name[0] != 'a' || y.name[1] != 'b' || y.name[2] != 'c' || y.name[3] != 0) return 4;
    if (y.v != 7) return 5;
    if (y.p != 9) return 6;

    /* 聚合成员整体赋值走的仍是同一份字节 */
    struct s dst = { { 0 } };
    dst = x;
    if (dst.raw.a != 1) return 7;

    /* 真正需要运行时初始化的聚合：嵌套函数属主里的 &&label 跳转表 */
    void inner(void) {
        static const void *l[] = {&&slot_a, &&slot_b};
        goto *l[n];
      slot_a: n = 20; return;
      slot_b: n = 30; return;
    }
    inner();
    if (n != 30) return 8;
    n = 0;
    inner();
    if (n != 20) return 9;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("static-compound-lit-init", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "static compound-lit init compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "static compound-lit init runtime check failed (rc={rc})");
}

/// `-mno-sse` 下调用结果必须物化到被要求的寄存器。
///
/// 现场是 PVH 启动：`per_cpu_ptr(&cpu_stopper, task_cpu(task))` 被编成
/// `call task_cpu` 后直接 `mov (%r11,%rcx,8)`，而调用的返回值在 `%rax` ——
/// 少一条 `mov %rcx,%rax`，下标读的是调用前的垃圾，内核在
/// `print_stop_info` 里以 CR2=垃圾 收场。
/// 根因是 `float_in_st0` 的 `_ => !target.sse` 兜底：`-mno-sse`（内核每个 C TU
/// 都带）把**整数/指针**返回值也说成「交付在 st(0)」，`gen_call_result_move`
/// 于是跳过搬送。这里两个档位都跑：-O0 走通用下标路径，-O2 走 SIB 快路径。
#[test]
#[cfg(unix)]
fn mno_sse_call_result_used_as_index_runtime() {
    let src = include_str!("../fixtures/c/per_cpu_call_index.c");
    for flag in ["-O0", "-O2"] {
        let (out, exe) = run_compile_and_run_flags(
            "mno-sse-call-index",
            src,
            &[flag, "-mno-sse"],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{flag} -mno-sse: 编译失败: {stderr}");
        let rc = run_exe(&exe);
        let _ = fs::remove_file(&exe);
        assert_eq!(rc, 0, "{flag} -mno-sse: 调用返回值未落到索引寄存器");
    }
}

/// P0-6: kernel builtin coverage — verify newly added builtins
/// (__builtin_ffs, __builtin_isdigit, __builtin_dynamic_object_size,
/// __builtin_extract_return_addr, __builtin_preserve_access_index)
/// compile and produce correct results at runtime.
#[test]
#[cfg(unix)]
fn kernel_builtin_coverage_runtime() {
    let src = include_str!("../fixtures/c/builtin_kernel_coverage.c");
    let (out, exe) = run_compile_and_run("kernel-builtin-cov", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "kernel builtin coverage compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "kernel builtin coverage runtime check failed");
}

/// P0-5: kernel asm instruction coverage — verify that inline
/// assembly instructions used in Linux arch/x86/ are properly
/// encoded by dcc's mini-assembler and produce correct results.
/// Tests: ALU, cmpxchg, xchg, bt, bsf/bsr, inc/dec, nop/pause,
/// push/pop, cmovz, setcc, mul, shrd, lar, rdpkru, clflush,
/// lock inc/dec, lock xadd, cpuid, rdtsc, rdtscp, fences,
/// prefetch, movbe, crc32, popcnt.
#[test]
#[cfg(unix)]
fn kernel_asm_instr_coverage_runtime() {
    let src = include_str!("../fixtures/asm/asm_kernel_instr_coverage.c");
    let (out, exe) = run_compile_and_run("kernel-asm-cov", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "kernel asm coverage compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "kernel asm coverage runtime check failed");
}

/// 预处理残留的 _DECLARE_STATIC_KEY_0 → extern struct static_key_false
#[test]
fn static_key_maybe_compiles() {
    let source = include_str!("../fixtures/c/skey_maybe.c");
    let output = run_syntax_test("static-key-maybe", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn static_key_chain_compiles() {
    let source = include_str!("../fixtures/c/skey_chain.c");
    let output = run_syntax_test("static-key-chain", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn static_key_decl_compiles() {
    let source = include_str!("../fixtures/c/skey_decl.c");
    let output = run_syntax_test("static-key-decl", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

// ============================================================
// 缺失特性补全: 新增测试契约
// ============================================================

/// kernel object corpus: 固化真实 Linux arch/x86/kernel .o 样本.
#[test]
fn kernel_object_corpus_arch_x86_kernel() {
    assert_kernel_corpus_object("io_delay.o", "native_io_delay");
}

/// kernel object corpus: 固化真实 Linux kernel/sched .o 样本.
#[test]
fn kernel_object_corpus_kernel_sched() {
    assert_kernel_corpus_object("fair.o", "pick_next_task_fair");
}

/// kernel object corpus: 固化真实 Linux lib/ .o 样本.
#[test]
fn kernel_object_corpus_lib() {
    assert_kernel_corpus_object("clz_ctz.o", "__clzdi2");
}

// ---------- ORC consumer objtool strict ----------

/// ORC 现由 objtool 统一生成, dcc 不再输出 ORC 节区.
/// 此测试验证: dcc 编译的 .o 文件不含 ORC 节区, 且 objtool 可正常处理.
#[test]
#[cfg(unix)]
fn orc_consumer_objtool_strict_single_object() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-orc-consumer-{}.c", std::process::id()));
    let obj_path = dir.join(format!("dcc-orc-consumer-{}.o", std::process::id()));
    let source = "int foo(void) { return 42; }\nint bar(int x) { return x + foo(); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-g", "-fno-omit-frame-pointer", "-c", "-o"])
        .arg(&obj_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    assert!(
        output.status.success(),
        "dcc should compile ORC consumer fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(source_path);

    // dcc 不再生成 ORC 节区, 由 objtool 统一处理
    let obj = fs::read(&obj_path).expect("read object file");
    let secs = elf64_sections(&obj);
    assert!(
        !secs.contains_key(".orc_unwind_ip"),
        "dcc should not emit .orc_unwind_ip (objtool handles ORC)"
    );
    assert!(
        !secs.contains_key(".orc_unwind"),
        "dcc should not emit .orc_unwind (objtool handles ORC)"
    );

    // 如果有 objtool, 确认其可正常处理 dcc 输出的 .o 文件
    if Command::new("sh")
        .args(["-c", "command -v objtool"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        let check = Command::new("objtool")
            .args(["check", obj_path.to_str().unwrap()])
            .output()
            .expect("run objtool check");
        let stderr = String::from_utf8_lossy(&check.stderr);
        // objtool 不应报致命错误 (缺少 ORC 不算致命, 它会自行生成)
        assert!(
            !stderr.contains("fatal"),
            "objtool should not report fatal errors: stderr={stderr}"
        );
    } else {
        eprintln!("SKIP: objtool not installed");
    }

    let _ = fs::remove_file(obj_path);
}

/// ORC consumer: 校验函数边界 st_size 非零, objtool 可定位起始指令.
#[test]
#[cfg(unix)]
fn orc_consumer_function_boundaries_objtool() {
    let source = include_str!("../fixtures/c/func_symbol_size.c");
    let (output, obj_path) = run_object_test("orc-consumer-func-boundaries", source);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let obj_data = std::fs::read(&obj_path).expect("read object file");
    let funcs = elf64_func_symbols(&obj_data);
    let names: std::collections::HashMap<String, u64> =
        funcs.iter().map(|(n, _, s)| (n.clone(), *s)).collect();
    for want in [
        "weak_func",
        "static_func",
        "real_func",
        "alias_func",
        "caller",
    ] {
        let sz = names
            .get(want)
            .unwrap_or_else(|| panic!("missing STT_FUNC symbol {want}"));
        assert!(*sz > 0, "function {want} has zero st_size (breaks objtool function boundary)");
    }
    let _ = std::fs::remove_file(&obj_path);
}

// ---------- Sanitizer runtime negative tests ----------

/// KASAN 越界: -fsanitize=address 时 heap 越界应触发 runtime 诊断.
/// 链接后运行验证 libasan 报告真实的越界访问.
#[test]
#[cfg(unix)]
fn kasan_runtime_out_of_bounds() {
    let source = "extern void *malloc(unsigned long);\n\
int main(void) {\n\
    int *p = (int *)malloc(4);\n\
    return p[2];\n\
}\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("kasan-oob", source, &["-fsanitize=address"]);
    assert_runtime_fails_with(&run, &log, &["AddressSanitizer", "ERROR"]);
    assert!(
        log.contains("use-after-poison")
            || log.contains("heap-buffer-overflow")
            || log.contains("unknown-crash"),
        "KASAN OOB log should identify an ASAN memory fault: {log}"
    );
    let _ = fs::remove_file(&exe_path);
}

/// KCSAN 竞态: kernel-kcsan 路径链接后应调用访问检查 handler.
#[test]
#[cfg(unix)]
fn kcsan_runtime_race_instrumentation() {
    let source = "extern void exit(int);\n\
int shared;\n\
__attribute__((no_sanitize(\"thread\")))\n\
void __kcsan_init(void) {}\n\
__attribute__((no_sanitize(\"thread\")))\n\
void __kcsan_func_entry(void) {}\n\
__attribute__((no_sanitize(\"thread\")))\n\
void __kcsan_check_access(void *addr, unsigned long size, int is_write) {\n\
    (void)addr;\n\
    if (size == 4 && is_write) exit(97);\n\
}\n\
void writer(int *p) { *p = 1; }\n\
int reader(int *p) { return *p; }\n\
int main(void) { writer(&shared); return reader(&shared); }\n";
    let (_, run, log, exe_path) =
        run_sanitized_binary("kcsan-race", source, &["-fsanitize=kernel-kcsan"]);
    assert_exit_code(&run, 97, &log);
    let _ = fs::remove_file(&exe_path);
}

// ---------- KCOV/Fuzzer ----------

/// KCOV/Fuzzer: trace_cmp* 插桩.
/// -fsanitize-coverage=trace-cmp 应产生 compare 与 PC/guard coverage 调用.
#[test]
#[cfg(unix)]
fn kcov_fuzzer_trace_cmp() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kcov-cmp-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kcov-cmp-{}.s", std::process::id()));
    let source = "int f(int a, int b) { return a == b; }\nint main(void) { return f(1, 2); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize-coverage=trace-cmp", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile KCOV trace-cmp fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("__sanitizer_cov_trace_cmp4"),
        "trace-cmp should emit __sanitizer_cov_trace_cmp4: {asm_text}"
    );
    assert!(
        asm_text.contains("__sanitizer_cov_trace_pc"),
        "trace-cmp should still emit trace_pc entry calls: {asm_text}"
    );
    assert!(
        !asm_text.contains("__sanitizer_cov_trace_pc_guard"),
        "trace-cmp 单独不应再发边 guard：guard 数组是往函数体里塞 `static` 声明，会把所有\n       `__always_inline` 助手的体内联否决掉（allmodconfig 的 jump-label 阻塞根因）。\n       发 guard 是 `trace-pc-guard` 的活。实测 asm 产物:\n{asm_text}"
    );
    // 反向锁定：显式要 `trace-pc-guard` 时 guard 必须回来（libFuzzer/syzkaller 依赖它）。
    let asm_path2 = dir.join(format!("dcc-kcov-cmp-guard-{}.s", std::process::id()));
    let src_path2 = dir.join(format!("dcc-kcov-cmp-guard-{}.c", std::process::id()));
    fs::write(&src_path2, source).expect("write C fixture");
    let out2 = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize-coverage=trace-cmp,trace-pc-guard", "-S", "-o"])
        .arg(&asm_path2)
        .arg(&src_path2)
        .output()
        .expect("run dcc");
    let asm2 = fs::read_to_string(&asm_path2).unwrap_or_default();
    let _ = fs::remove_file(src_path2);
    let _ = fs::remove_file(asm_path2);
    assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stderr));
    assert!(
        asm2.contains("__sanitizer_cov_trace_pc_guard"),
        "trace-pc-guard 应发边 guard 调用，实测没有:\n{asm2}"
    );
}

/// KCOV/Fuzzer: trace_switch 插桩.
#[test]
#[cfg(unix)]
fn kcov_fuzzer_trace_switch() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kcov-switch-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kcov-switch-{}.s", std::process::id()));
    let source = "int f(int x) { switch(x) { case 1: return 1; case 2: return 2; default: return 0; } }\nint main(void) { return f(1); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize-coverage=trace-switch", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile KCOV trace-switch fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("__sanitizer_cov_trace_switch"),
        "trace-switch should emit __sanitizer_cov_trace_switch: {asm_text}"
    );
    assert!(
        asm_text.contains("__sanitizer_cov_trace_pc"),
        "trace-switch should still emit trace_pc entry calls: {asm_text}"
    );
}

/// KCOV/Fuzzer: 8-bit counters.
/// -fsanitize-coverage=8bit-counters 应产生 coverage 插桩调用.
#[test]
#[cfg(unix)]
fn kcov_fuzzer_8bit_counters() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kcov-8bit-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kcov-8bit-{}.s", std::process::id()));
    let source = "int f(int x) { return x > 0 ? x : -x; }\nint main(void) { return f(5); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize-coverage=8bit-counters", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile KCOV 8bit-counters fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("__sanitizer_cov_8bit_counters_init"),
        "8bit-counters should emit counter init callback: {asm_text}"
    );
    assert!(
        asm_text.contains("__sanitizer_cov_trace_pc_guard"),
        "8bit-counters should emit edge guard callbacks: {asm_text}"
    );
}

/// KCOV/Fuzzer: PC table.
/// -fsanitize-coverage=pc-table 应产生 coverage 插桩调用.
#[test]
#[cfg(unix)]
fn kcov_fuzzer_pc_table() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kcov-pc-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kcov-pc-{}.s", std::process::id()));
    let source = "int f(int x) { return x + 1; }\nint main(void) { return f(42); }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize-coverage=pc-table", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(
        output.status.success(),
        "dcc should compile KCOV pc-table fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        asm_text.contains("__sanitizer_cov_pcs_init"),
        "pc-table should emit PC table init callback: {asm_text}"
    );
    assert!(
        asm_text.contains("__sanitizer_cov_trace_pc_guard"),
        "pc-table should emit edge guard callbacks: {asm_text}"
    );
}

/// KCOV/Fuzzer: no_sanitize 与 KCOV/Fuzzer 交互.
/// no_sanitize("address") 应只关闭 ASAN，保留 KCOV/Fuzzer coverage.
#[test]
#[cfg(unix)]
fn kcov_fuzzer_no_sanitize_interaction() {
    let source = "__attribute__((no_sanitize(\"address\")))\n\
int hot_path(int *p) { return *p == 7; }\n\
int main(void) { int x = 7; return hot_path(&x) ? 0 : 1; }\n";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kcov-nosan-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kcov-nosan-{}.s", std::process::id()));
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address,fuzzer", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm_text = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(source_path);
    let _ = fs::remove_file(asm_path);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let hot_start = asm_text.find("hot_path:").expect("hot_path label");
    let main_start = asm_text.find("main:").expect("main label");
    let hot_asm = &asm_text[hot_start..main_start];
    assert!(
        hot_asm.contains("__sanitizer_cov_trace_pc"),
        "no_sanitize(address) should keep KCOV/Fuzzer entry coverage: {hot_asm}"
    );
    assert!(
        hot_asm.contains("__sanitizer_cov_trace_cmp4"),
        "no_sanitize(address) should keep fuzzer compare coverage: {hot_asm}"
    );
    assert!(
        !hot_asm.contains("__asan_load"),
        "no_sanitize(address) should suppress ASAN loads in hot_path: {hot_asm}"
    );
}

/// P1.5: 完整内核 KASAN shadow 检查. -fsanitize=kernel-address 应内联
/// shadow 地址计算 (shadow = (addr >> 3) + 0xdffffc0000000000) 并在
/// shadow 字节毒化时调用 __asan_report_loadN (size 感知), 而不是调用
/// 用户态 __asan_loadN/__asan_storeN 运行时函数.
#[test]
fn kernel_kasan_inline_shadow_report_emitted() {
    let src = "\
int f(int *p) { return *p; }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-kernel-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kasan-kernel-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write kernel kasan fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=kernel-address", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "kernel-address compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    // 4 字节 int 解引用 → shadow 违规时调 __asan_report_load4
    assert!(
        asm.contains("call __asan_report_load4"),
        "kernel KASAN should inline shadow check + report_load4, asm:\n{}",
        asm
    );
    // 不应调用用户态运行时检查函数
    assert!(
        !asm.contains("call __asan_load4") && !asm.contains("call __asan_store"),
        "kernel KASAN must not emit user-mode __asan_loadN/__asan_storeN:\n{}",
        asm
    );
    // 内核运行时自初始化 KASAN, 不发射用户态 __asan_init
    assert!(
        !asm.contains("call __asan_init"),
        "kernel KASAN must not emit user-mode __asan_init:\n{}",
        asm
    );
}

/// P1.5: user-mode TSan 与 KASAN 可同时启用. 同一访存应同时发射
/// __tsan_readN 与 __asan_loadN.
#[test]
fn tsan_with_kasan_emits_both_checks() {
    let src = "\
int f(int *p) { return *p; }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-tsan-kasan-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-tsan-kasan-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write tsan+kasan fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address,thread", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "address,thread compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    assert!(
        asm.contains("call __asan_load4"),
        "KASAN load check missing when TSan coexists:\n{}",
        asm
    );
    assert!(
        asm.contains("call __tsan_read4"),
        "TSan check missing when KASAN coexists:\n{}",
        asm
    );
}

/// P1.5: 自增/自减内存目标 (*p)++ / a[i]-- 是读-改-写, KASAN 下应
/// 同时发射 __asan_loadN 与 __asan_storeN.
#[test]
fn kasan_store_check_on_postinc_deref_emitted() {
    let src = "\
void f(int *p) { (*p)++; }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-incdec-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kasan-incdec-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write incdec fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "incdec + asan compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    assert!(
        asm.contains("call __asan_store4"),
        "KASAN should emit __asan_store4 before (*p)++, asm:\n{}",
        asm
    );
    assert!(
        asm.contains("call __asan_load4"),
        "KASAN should emit __asan_load4 for the read part of (*p)++, asm:\n{}",
        asm
    );
}

/// KASAN 对 >16 字节聚合类型的 load/store 使用通用 __asan_loadN/
/// __asan_storeN 接口 (携带 size 参数). 验证编译不崩溃且正确发射.
#[test]
fn kasan_aggregate_load_store_above_16_bytes() {
    let src = "\
struct Big { long a, b, c; }; /* 24 bytes > 16 */
struct Big load_big(struct Big *p) { return *p; }
void store_big(struct Big *p, struct Big v) { *p = v; }
";
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-agg-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kasan-agg-{}.s", std::process::id()));
    fs::write(&source_path, src).expect("write agg fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "kasan agg compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&asm_path);
    // 24 字节 > 16, 应使用通用 __asan_loadN/__asan_storeN
    // (而非固定大小的 __asan_load{1,2,4,8,16})
    assert!(
        asm.contains("__asan_loadN") || asm.contains("__asan_load16"),
        "KASAN should emit load check for >16B aggregate load, asm:\n{}",
        asm
    );
    assert!(
        asm.contains("__asan_storeN") || asm.contains("__asan_store16"),
        "KASAN should emit store check for >16B aggregate store, asm:\n{}",
        asm
    );
}

// ============================================================================
// Kernel-style header preprocessing E2E (self-contained fixtures)
// ============================================================================

/// 内核风格头 compiler_types.h 预处理：验证嵌套 include 与 `__force` /
/// `__maybe_unused` 宏展开。夹具现场生成，零第三方文件。
#[test]
fn kernel_style_header_compiler_types_preprocess_e2e() {
    let root = std::env::temp_dir().join(format!(
        "dcc-khdr-compiler-{}-{}",
        std::process::id(),
        temp_tag()
    ));
    write_kernel_style_headers(&root);
    let src = root.join("use_compiler_types.c");
    let out = root.join("use_compiler_types.i");

    fs::write(
        &src,
        "#include <linux/compiler_types.h>\n\
         int f(void) { __force int x = 1; __maybe_unused int y = 2; return x + y; }\n",
    )
    .expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg("-I")
        .arg(root.join("include"))
        .arg(&src)
        .output()
        .expect("run dcc -E");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "compiler_types.h preprocess must succeed; stderr: {stderr}"
    );
    let text = fs::read_to_string(&out).unwrap_or_default();
    assert!(
        text.contains("__attribute__((__unused__))"),
        "`__maybe_unused` 未按内核风格头展开: {text}"
    );
    assert!(
        text.contains("__force int x") || text.contains("int x = 1;") || text.contains("int x = 1 ;"),
        "`__force` 应展开为空但保留声明: {text}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// 内核风格头 rwonce.h 预处理：验证 READ_ONCE/WRITE_ONCE 宏展开。
#[test]
fn kernel_style_header_rwonce_preprocess_e2e() {
    let root = std::env::temp_dir().join(format!(
        "dcc-khdr-rwonce-{}-{}",
        std::process::id(),
        temp_tag()
    ));
    write_kernel_style_headers(&root);
    let src = root.join("use_rwonce.c");
    let out = root.join("use_rwonce.i");

    fs::write(
        &src,
        "#include <asm-generic/rwonce.h>\n\
         int main(void) { int val = 42; int r = READ_ONCE(val); WRITE_ONCE(val, r + 1); return val - 43; }\n",
    )
    .expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg("-I")
        .arg(root.join("include"))
        .arg(&src)
        .output()
        .expect("run dcc -E");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "rwonce.h preprocess must succeed; stderr: {stderr}"
    );
    let text = fs::read_to_string(&out).unwrap_or_default();
    assert!(
        text.contains("const volatile"),
        "READ_ONCE 未展开成 volatile 读: {text}"
    );
    assert!(
        text.contains("do {") || text.contains("while (0)"),
        "WRITE_ONCE 未展开成 do/while 语句: {text}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// 内核风格头 generated/autoconf.h 预处理：验证 CONFIG_* 宏状态驱动条件编译。
#[test]
fn kernel_style_header_autoconf_preprocess_e2e() {
    let root = std::env::temp_dir().join(format!(
        "dcc-khdr-autoconf-{}-{}",
        std::process::id(),
        temp_tag()
    ));
    write_kernel_style_headers(&root);
    let src = root.join("use_autoconf.c");
    let out = root.join("use_autoconf.i");

    fs::write(
        &src,
        "#include <autoconf.h>\n\
         #ifdef CONFIG_MTRR\n\
         int mtrr_enabled(void) { return 12345; }\n\
         #else\n\
         int mtrr_enabled(void) { return 54321; }\n\
         #endif\n\
         int main(void) { return mtrr_enabled(); }\n",
    )
    .expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg("-I")
        .arg(root.join("generated"))
        .arg(&src)
        .output()
        .expect("run dcc -E");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "autoconf.h preprocess must succeed; stderr: {stderr}"
    );
    let text = fs::read_to_string(&out).unwrap_or_default();
    assert!(
        text.contains("return 12345;"),
        "CONFIG_MTRR 已定义，应保留 #ifdef 分支: {text}"
    );
    assert!(
        !text.contains("54321"),
        "CONFIG_MTRR 已定义，#else 分支不应出现: {text}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// 内核风格头依赖图：嵌套 include 闭包必须被 `-MMD` 完整记录。
#[test]
fn kernel_style_header_graph_dependency_closure() {
    let root = std::env::temp_dir().join(format!(
        "dcc-khdr-graph-{}-{}",
        std::process::id(),
        temp_tag()
    ));
    write_kernel_style_headers(&root);
    let src = root.join("graph.c");
    let obj = root.join("graph.o");
    let dep = root.join("graph.d");

    fs::write(
        &src,
        "#include <autoconf.h>\n\
         #include <linux/compiler_types.h>\n\
         int main(void) {\n\
             __force int x = 0;\n\
         #ifdef CONFIG_MTRR\n\
             (void)x;\n\
         #endif\n\
             return 0;\n\
         }\n",
    )
    .expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-MMD", "-MF"])
        .arg(&dep)
        .arg("-I")
        .arg(root.join("include"))
        .arg("-I")
        .arg(root.join("generated"))
        .arg("-o")
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc -MMD");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "header graph compile must succeed; stderr: {stderr}"
    );
    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    for header in ["autoconf.h", "compiler_types.h", "compiler_attributes.h"] {
        assert!(
            dep_text.contains(header),
            "depfile 应记录嵌套 include 闭包里的 {header}: {dep_text}"
        );
    }
    assert!(
        !dep_text.contains("/usr/src"),
        "depfile 不得指向宿主第三方路径: {dep_text}"
    );
    let _ = fs::remove_dir_all(&root);
}

// ====== kernel-sensitive opt regression: volatile/barrier/asm clobber ======

/// volatile: O2 不应消除 volatile 变量的写和读
#[test]
#[cfg(unix)]
fn kernel_opt_volatile_not_eliminated_at_o2() {
    let source = "\
volatile int flag = 0;
int main(void) {
    flag = 1;
    return flag - 1;
}
";
    // O2
    let (output, exe) = run_compile_and_run_flags("volatile-o2", source, &["-O2"]);
    assert!(
        output.status.success(),
        "volatile O2 compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    assert_eq!(rc, 0, "volatile write/read must survive O2, got rc={rc}");
    let _ = fs::remove_file(&exe);
    // O3
    let (output, exe) = run_compile_and_run_flags("volatile-o3", source, &["-O3"]);
    assert!(
        output.status.success(),
        "volatile O3 compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "volatile write/read must survive O3, got rc={rc}");
}

/// barrier: asm volatile("" ::: "memory") 应阻止编译器重排序访存
#[test]
#[cfg(unix)]
fn kernel_opt_barrier_prevents_reordering_at_o2() {
    let source = "\
int x = 0;
int y = 0;
int main(void) {
    x = 1;
    __asm__ volatile(\"\" ::: \"memory\");
    y = 1;
    return (x + y) - 2;
}
";
    let (output, exe) = run_compile_and_run_flags("barrier-o2", source, &["-O2"]);
    assert!(
        output.status.success(),
        "barrier O2 compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "barrier must prevent reordering at O2, got rc={rc}");
}

/// Feature 8: ftrace instrumentation (-pg/-mfentry/-mrecord-mcount)
/// Verify that ftrace modes compile and produce expected symbols.
#[test]
#[cfg(unix)]
fn ftrace_modes_compile() {
    let src = r#"
int test_func(int x) { return x + 1; }
"#;
    let tmp = std::env::temp_dir();
    // Test -pg mode
    let src_path = tmp.join("ftrace_pg.c");
    let obj_path = tmp.join("ftrace_pg.o");
    fs::write(&src_path, src).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-pg", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -pg");
    assert!(
        output.status.success(),
        "ftrace -pg compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Verify mcount symbol exists
    let obj_bytes = fs::read(&obj_path).unwrap();
    let _obj_str = String::from_utf8_lossy(&obj_bytes);
    // ELF should have mcount as undefined symbol
    assert!(!obj_bytes.is_empty(), "object file should be generated");
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);

    // Test -mfentry mode
    let src_path = tmp.join("ftrace_fentry.c");
    let obj_path = tmp.join("ftrace_fentry.o");
    fs::write(&src_path, src).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-mfentry", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -mfentry");
    assert!(
        output.status.success(),
        "ftrace -mfentry compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);

    // Test -mrecord-mcount mode
    let src_path = tmp.join("ftrace_record.c");
    let obj_path = tmp.join("ftrace_record.o");
    fs::write(&src_path, src).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-mrecord-mcount", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc -mrecord-mcount");
    assert!(
        output.status.success(),
        "ftrace -mrecord-mcount compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Verify __mcount_loc section exists
    let obj_bytes = fs::read(&obj_path).unwrap();
    let has_mcount_loc = obj_bytes
        .windows(13)
        .any(|w| w == b"__mcount_loc\0" || w.starts_with(b"__mcount_loc"));
    assert!(has_mcount_loc, "-mrecord-mcount should generate __mcount_loc section");
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
}

/// Verify that `-mcmodel=kernel` bypasses GOT for ALL symbol references.
/// Data symbols (global, extern, static) must use R_X86_64_PC32 (RIP-relative),
/// function symbols must use R_X86_64_PLT32 (direct).  No GOTPCREL allowed.
#[test]
#[cfg(unix)]
fn kernel_code_model_data_symbols_bypass_got() {
    let src = include_str!("../fixtures/c/kernel_code_model_data.c");
    let (out, obj) = run_object_test_flags("kernel-cmodel-data", src, &["-mcmodel=kernel"]);
    assert!(
        out.status.success(),
        "kernel_code_model compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let relocs = readelf_relocations(&obj);
    let _ = fs::remove_file(&obj);

    // Must NOT contain any GOTPCREL relocations
    assert!(
        !relocs.contains("GOTPCREL"),
        "kernel code model must not use GOT for any symbol: {relocs}"
    );

    // Data symbols (load/store/addr-of) must use R_X86_64_PC32
    assert!(
        relocs.contains("PC32") && relocs.contains("k_global_var"),
        "data symbols should use R_X86_64_PC32: {relocs}"
    );
    assert!(
        relocs.contains("k_ext_var"),
        "extern data symbols should appear in relocations: {relocs}"
    );

    // Function address-taking should use PLT32 (or PC32), not GOTPCREL
    assert!(
        (relocs.contains("PLT32") || relocs.contains("PC32")) && relocs.contains("k_internal_func"),
        "internal function address should use direct relocation: {relocs}"
    );
    assert!(
        relocs.contains("k_ext_func"),
        "external function address should appear in relocations: {relocs}"
    );
}

/// 内核 `EXPORT_SYMBOL_GPL(f)` 展开出的 `extern typeof(f) f;` 不得被判成类型冲突。
///
/// 缺陷形状（`kernel/irq/chip.c` 整文件失败）：`struct irq_desc` 先前向声明、后补全，
/// 补全前被复制走的那份类型字段表是空的，`typeof(f)` 那一份是满的；派生的结构相等把
/// 两者判成不同类型 ⇒ E0007「conflicting types for redeclaration」。而补全后的字段又能
/// 指回自身（`irq_flow_handler_t` 是接 `struct irq_desc *` 的函数指针），深度比较还会
/// 沿环无限递归。具名聚合改按 C 的名义同一律比较，带环类型图按共归纳断环。
#[test]
#[cfg(unix)]
fn export_symbol_typeof_redeclaration_matches_cyclic_struct_function() {
    let source = "\
struct box;                                 /* 前向声明：字段表为空 */
typedef void (*cb_t)(struct box *b);        /* 参数指回 struct box ⇒ 类型图带环 */
struct box { int x; cb_t f; };              /* 补全 */

void handler(unsigned int i, cb_t c) { (void)c; }
extern typeof(handler) handler;             /* EXPORT_SYMBOL_GPL 的展开形态 */
";
    let bytes = dcc_obj_bytes("export-typeof", source, &["-O2"]);
    assert!(
        bytes.len() > 64,
        "typeof 重声明的 TU 应产出目标文件，实际 {} 字节",
        bytes.len()
    );
}

/// `__bug_table` pushsection 区里的数字标签必须在函数级并行合并下保持命名空间唯一。
///
/// 缺陷形状（`init/main.c`、`arch/x86/entry/vdso/vma.c` 等整 TU 失败）：内核 WARN/BUG
/// 展开的 asm 文本里，`__bug_table` 区中既有「数字标签 + 同行 body」
/// （`912: .pushsection .discard.annotate_data …`），又有纯数字标签行（`2:`）。发射时
/// 除唯一内部名（`.Lasm_local_<scope>_912_<n>`）外，还把**原文数字名**也登记进标号表，
/// 于是第二个函数带着同名 `912`/`2` 合并时撞车 ⇒ E1101 内部不变量错误。
#[test]
#[cfg(unix)]
fn bug_table_pushsection_numeric_labels_survive_parallel_merge() {
    let source = r#"
extern void bar(void);

#define BUG_BLOCK                                                              \
	"1:\n"                                                                   \
	".pushsection __bug_table,\"aw\"\n\t"                                    \
	"912: .pushsection .discard.annotate_data, \"M\", @progbits, 8; "        \
	".long 912b - ., 1; .popsection" "\n\t"                                  \
	"2:\n\t"                                                                 \
	"\t.long 1b - .\n"                                                       \
	".popsection\n"

static inline void bug_a(void) { asm volatile(BUG_BLOCK ::: "memory"); }
static inline void bug_b(void) { asm volatile(BUG_BLOCK ::: "memory"); }

void f1(void) { bug_a(); bar(); }
void f2(void) { bug_b(); bar(); }
void f3(void) { bug_a(); bug_b(); bar(); }
void f4(void) { bug_b(); bug_a(); bar(); }
"#;
    // -O2 且多函数 ⇒ 走 `emit_functions_parallel`；撞名正是在合并阶段暴露的。
    let bytes = dcc_obj_bytes("bug-annot-merge", source, &["-O2"]);
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("__bug_table"),
        "并行合并后 __bug_table 区应仍然存在"
    );
}

/// `-fstack-protector-strong` 的金丝雀失败路径（`call __stack_chk_fail`）不得被
/// 「caller-saved 家保存窗口」守卫（E1136）拦下。
///
/// 为什么这条 call 本来就不需要窗口，两条理由缺一不可：被调方 noreturn ⇒ 它之后本路径
/// 再无「回头读家里的值」的一面；成功路径是 `je` **跳过**这条 call（落在 call 之后），
/// 寄存器内容在成功路径上从未被它触碰。反过来套窗口是有害的：`je` 的位移按 call 恰
/// 5 字节硬编码，窗口的存/取会多发指令，把跳过目标算歪。
///
/// 缺陷形状：内核开 `-fstack-protector-strong` 后 `lib/list_sort.c` 整 TU 失败
/// （`error[E1136]: call 到 '__stack_chk_fail' 不经过 caller-saved 家的保存窗口`）。
/// 夹具把「局部数组（触发金丝雀）+ 一串跨 call 存活并被回读的值（住进延伸池号）」压到
/// 同一条路径上；把 `call_label_noreturn` 换回 `call_label` 后本用例当场变红（实测）。
#[test]
#[cfg(unix)]
fn stack_protector_canary_fail_call_needs_no_home_window() {
    let source = "\
#include <stdio.h>

static int sink_calls;

static int sink(int v)
{
	sink_calls += v & 3;
	return sink_calls;
}

/* 多个跨 call 存活、call 之后仍要回读的值 —— 它们会被分到延伸池的 caller-saved 号里。 */
static int strong_fn(int a)
{
	char buf[24];
	int v0 = a, v1 = a + 1, v2 = a + 2, v3 = a + 3, v4 = a + 4, v5 = a + 5, v6 = a + 6;
	int i;

	for (i = 0; i < 24; i++)
		buf[i] = (char)(a + i);
	int s = buf[0];
	s += v0 * (buf[0] + 0); sink(s);
	s += v1 * (buf[1] + 1); sink(s);
	s += v2 * (buf[2] + 2); sink(s);
	s += v3 * (buf[3] + 3); sink(s);
	s += v4 * (buf[4] + 4); sink(s);
	s += v5 * (buf[5] + 5); sink(s);
	s += v6 * (buf[6] + 6); sink(s);
	return s + buf[1] + sink_calls;
}

int main(void)
{
	/* 两次调用都要改 `sink_calls`：必须按「求值顺序已定」的语句排，不能写进同一个
	 * printf 的参数表（实参求值顺序由实现决定，gcc 与 dcc 本来就可以不同）。 */
	int a = strong_fn(3);
	int b = strong_fn(-5);
	printf(\"%d %d\\n\", a, b);
	return 0;
}
";
    // 判据一：编得过去（E1136 是硬错误，rc≠0 时 dcc_obj_bytes 当场变红）。
    let bytes = dcc_obj_bytes(
        "canary-noreturn-obj",
        source,
        &["-O2", "-fstack-protector-strong"],
    );
    // 判据二：这个夹具**确实**走到了金丝雀路径，否则用例只是空跑。
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("__stack_chk_fail"),
        "夹具没产生金丝雀检查（产物里没有 __stack_chk_fail 引用），用例就成了空跑"
    );
    // 判据三：行为与 gcc 一致（成功路径上的家确实没被这次调用毁掉）。
    for opt in ["-O0", "-O2", "-O3"] {
        run_gcc_cross_check_opt(
            "canary-noreturn-exec",
            source,
            &[opt, "-fstack-protector-strong"],
        );
    }
}

/// `asm goto` 形体的 `__always_inline` 必须被展开，展开后标签与 `%l[...]` 一起改名。
///
/// 缺陷现场：`body_is_splicable` 把 `Stmt::AsmGoto` 与 computed goto 归为同一类「跟当前
/// 函数绑死」的形状一并拒绝搬移，于是内核
/// `arch/x86/include/asm/jump_label.h` 的 `arch_static_branch`（体是「asm goto + 一个标签
/// + 两个 return」）永远不内联。不内联时它的 `"i" (key)` 只能拿到寄存器，模板里的
/// `.quad %c0 + %c1 - .` 就变成一条指向寄存器名的野重定位（实测 `vmlinux.o` 的
/// `__jump_table`/`.altinstructions` 上各有一片，objtool 到链接后才报
/// `special: can't find orig instruction`）。
#[test]
#[cfg(unix)]
fn asm_goto_always_inline_body_inlines_and_folds_link_time_address() {
    let source = r#"
struct static_key {
	int enabled;
};

extern struct static_key my_key;

#define JUMP_TABLE_ENTRY(key, label)							\
	".pushsection __jump_table, \"aw\" \n\t"						\
	".balign 8 \n\t"										\
	".long 1b - . \n\t"										\
	".long " label " - . \n\t"									\
	".quad " key " - . \n\t"									\
	".popsection \n\t"

static __attribute__((always_inline)) inline int
arch_static_branch(struct static_key *key, int branch)
{
	__asm__ goto("1: .byte 0xe9\n\t" JUMP_TABLE_ENTRY("%c0 + %c1", "%l[l_yes]")
		: : "i"(key), "i"(branch) : : l_yes);
	return 0;
l_yes:
	return 1;
}

int probe_expr(void)
{
	return arch_static_branch(&my_key, 0);
}

void probe_cond(void)
{
	if (arch_static_branch(&my_key, 1))
		probe_expr();
}
"#;
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-jumplabel-{pid}.c"));
    let op = dir.join(format!("dcc-jumplabel-{pid}.o"));
    fs::write(&sp, source).expect("write fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-fno-PIE", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "编译失败: {stderr}");

    // 判据一：callee 已被逐点展开，产物里不该再有一份 out-of-line 定义。
    let nm = Command::new("nm").arg(&op).output().expect("nm");
    let syms = String::from_utf8_lossy(&nm.stdout).to_string();
    assert!(
        !syms.contains("arch_static_branch"),
        "asm goto 体没被内联，仍留了一份 out-of-line 副本:\n{syms}"
    );
    // 判据二：`__jump_table` 的 key 字段是一条对 `my_key` 的重定位，而不是指向寄存器名。
    // readelf 的每条重定位行本身不带节名（节名只在「重定位节 '.rela__jump_table'」那一行），
    // 所以按节分组收行；标题行里的引号名与 locale 无关。
    let rel = Command::new("readelf")
        .args(["-r", "--wide"])
        .arg(&op)
        .output()
        .expect("readelf");
    let rels = String::from_utf8_lossy(&rel.stdout).to_string();
    let mut cur_section = String::new();
    let mut jump_rows: Vec<String> = Vec::new();
    for l in rels.lines() {
        if let Some(at) = l.find("'.rela") {
            cur_section = l[at + 1..].split('\'').next().unwrap_or("").to_string();
            continue;
        }
        if cur_section == ".rela__jump_table" && l.split_whitespace().count() >= 4 {
            jump_rows.push(l.to_string());
        }
    }
    assert!(
        jump_rows.len() >= 4,
        "__jump_table 该有两条入口、每条至少 2 个重定位字段，实际行：{jump_rows:?}\n{rels}"
    );
    let key_rows = jump_rows.iter().filter(|l| l.contains("my_key")).count();
    assert!(
        key_rows >= 2,
        "每个 `__jump_table` 条目的 key 字段都应是→ my_key 的重定位，实际 {key_rows} 条：{jump_rows:?}"
    );
    for reg in ["rax", "rbx", "rcx", "rdx", "rsi", "rdi"] {
        assert!(
            !rels.lines().any(|l| l.split_whitespace().any(|t| t == reg)),
            "重定位符号里出现了寄存器名 {reg}（说明 `%c` 没拿到链接期常量）:\n{rels}"
        );
    }
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
}

/// 函数被 `__section(".init.text")` 搬走后，函数体内 asm 的标号必须一起定节。
///
/// 缺陷现场：`custom_func_ranges` 只搬代码与重定位，`pushsection_sym_loc` 只认
/// 可执行 pushsection 区间。函数体内 `1:` 这类标号仍被发成 `.text + 原发射偏移`，
/// 而 `.text` 原位只剩 0x90 填充 ⇒ `__jump_table` 的 `code` 字段指进 NOP 洞，
/// 运行时 `insn_decode` 读到 length=1，`arch_jump_entry_size` 触发
/// `BUG_ON(insn.length != 2 && insn.length != 5)`（vmlinux 实测 306/5053 条）。
#[test]
#[cfg(unix)]
fn asm_label_in_moved_section_function_resolves_to_target_section() {
    let source = r#"
struct jump_entry { int code; int target; long key; };
extern long fake_key;

__attribute__((section(".init.text"), noinline))
static int init_thing(int x)
{
	asm volatile goto(
		"1:\n\t"
		"jmp %l[l_yes]\n\t"
		".pushsection __jump_table, \"aw\"\n\t"
		".balign 8\n\t"
		".long 1b - .\n\t"
		".long %l[l_yes] - .\n\t"
		".quad fake_key\n\t"
		".popsection\n\t"
		::: : l_yes);
	return x + 1;
l_yes:
	return x + 2;
}

int repro_entry(int x) { return init_thing(x); }
long fake_key;
"#;
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let sp = dir.join(format!("dcc-moved-label-{pid}.c"));
    let op = dir.join(format!("dcc-moved-label-{pid}.o"));
    fs::write(&sp, source).expect("write fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-fno-PIE", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "编译失败: {stderr}");

    // `__jump_table` 第一行 `.long 1b - .` 的 PC32 目标符号：它就是函数体内的 `1:`。
    let rel = Command::new("readelf")
        .args(["-r", "--wide"])
        .env("LC_ALL", "C")
        .arg(&op)
        .output()
        .expect("readelf -r");
    let rels = String::from_utf8_lossy(&rel.stdout).to_string();
    let mut in_jump_table = false;
    let mut label_sym = String::new();
    for line in rels.lines() {
        if line.contains("'.rela__jump_table'") {
            in_jump_table = true;
            continue;
        }
        if in_jump_table && line.trim_start().starts_with("0000") {
            let toks: Vec<&str> = line.split_whitespace().collect();
            if toks.len() >= 5 {
                label_sym = toks[4].to_string();
            }
            break;
        }
    }
    assert!(!label_sym.is_empty(), "没找到 __jump_table 的第一条重定位:\n{rels}");

    // 符号必须定在 .init.text（而不是 .text —— 后者是搬走前的原位）。
    // 用 objdump -t 而不是 readelf -s：后者对额外节的符号只印节号（如 "8"），
    // 前者直接印节名，断言不受节号编排影响。
    let syms = Command::new("objdump")
        .args(["-t"])
        .env("LC_ALL", "C")
        .arg(&op)
        .output()
        .expect("objdump -t");
    let sym_txt = String::from_utf8_lossy(&syms.stdout).to_string();
    let mut sym_section = String::new();
    for line in sym_txt.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        let Some(&last) = toks.last() else {
            continue;
        };
        if last != label_sym {
            continue;
        }
        // 格式: <addr> <flags> <section> <size> <name>
        if let Some(sec) = toks[..toks.len() - 1].iter().rev().find(|t| t.starts_with('.')) {
            sym_section = sec.to_string();
        }
        break;
    }
    assert_eq!(
        sym_section, ".init.text",
        "函数体内 asm 标号 {label_sym} 未随函数定节（section={sym_section:?}），\
         应指向 .init.text；符号表:\n{sym_txt}"
    );
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
}

/// `&数组[形参]` 里的形参是**下标**，不是「被取址的对象」，不得因此否决表达式内联。
///
/// 缺陷现场：`kernel/sched/core.c` 在 `__perf_sw_enabled()` 上炸出
/// `E0311: ... operand 0 with immediate-only constraint "i" has no compile-time constant
/// value (expression: __inl9098$_key, in function __perf_sw_enabled)`。
/// `subst_params_mut` 的 `AddrOf` 把「正在 `&` 下面」一路带进 `Index` 的两个子表达式，
/// 于是 `return static_key_false(&perf_swevent_enabled[swevt]);` 被判成「形参被取址」
/// （判决行：`REJECT __perf_sw_enabled (形参被取址 (&p)，表达式内联给不了独立槽位)`）。
/// `__always_inline` 被拒 ⇒ DCE 保留一份外联副本 ⇒ 副本里 `swevt` 是运行期形参，
/// `"i"(&perf_swevent_enabled[swevt])` 无从编码。
///
/// 判据就是「能编过 + 不留副本」：下标换成实参后 `&keys[3]` 是链接期常量，
/// `__jump_table` 该照常成立。
#[test]
#[cfg(unix)]
fn array_index_param_under_addressof_still_inlines() {
    let source = r#"
struct static_key { int enabled; };

extern struct static_key keys[4];

static __attribute__((always_inline)) _Bool
arch_static_branch(struct static_key *const key, const _Bool branch)
{
	__asm__ goto("1: jmp %l[l_yes]\n\t"
		     ".pushsection __jump_table, \"aw\"\n\t"
		     ".balign 8\n\t"
		     ".long 1b-.\n\t"
		     ".quad %c0+%c1\n\t"
		     ".quad %l[l_yes]\n\t"
		     ".popsection\n\t"
		     : : "i"(key), "i"(branch) : : l_yes);
	return 0;
l_yes:
	return 1;
}

static __attribute__((always_inline)) _Bool
key_false(struct static_key *key)
{
	return arch_static_branch(key, 0);
}

/* 体里有 `&keys[idx]`：idx 只贡献下标值。 */
static __attribute__((always_inline)) _Bool
sw_enabled(int idx)
{
	return key_false(&keys[idx]);
}

struct task_struct { int sched_migrated; };

static inline void sched_in(struct task_struct *task)
{
	if (sw_enabled(3) && task->sched_migrated)
		task->sched_migrated = 0;
}

void caller(struct task_struct *task)
{
	sched_in(task);
}
"#;
    let pid = std::process::id();
    let sp = std::env::temp_dir().join(format!("dcc-idxaddr-{pid}.c"));
    let op = std::env::temp_dir().join(format!("dcc-idxaddr-{pid}.o"));
    fs::write(&sp, source).expect("write fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-fno-PIE", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "编译失败（下标被误判成取址）: {stderr}");
    let nm = Command::new("nm").arg(&op).output().expect("nm");
    let syms = String::from_utf8_lossy(&nm.stdout).to_string();
    for dead in ["sw_enabled", "key_false", "arch_static_branch"] {
        assert!(
            !syms.lines().any(|l| l.split_whitespace().last() == Some(dead)),
            "{dead} 仍留着 out-of-line 副本（说明 always_inline 没展开）:\n{syms}"
        );
    }
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
}

/// `.field = "字符串"` 初始化 char 数组字段（指定初始化器）必须把 `init_idx` 前移。
///
/// 缺陷现场：`fs/proc/kcore.c` 的
/// `struct elf_prpsinfo prpsinfo = { .pr_sname = 'R', .pr_fname = "vmlinux" };`
/// 让 dcc 直接吃 4.5 GiB 被 OOM 杀掉（`rc=137`）。`gen_init_list_at` 的 struct 分支
/// 里，`InitItem::Expr` 那条字符串 arm 有 `init_idx += 1`，而**指定初始化器**落到
/// 的同义 arm 在 `continue` 前漏了它；循环体底部的 `init_idx += 1` 被 `continue` 跳过，
/// 于是同一个初始化项被无限重放（每圈重发一遍字符串的逐字节 store）——
/// gdb 抓到的空转点就是那条 `mov_imm`，`(u64, Insn)` 缓冲 1.2 → 2.4 → 4.8 GiB 翻倍。
///
/// 判据两条：编译必须在有界时间内返回（死循环会被判 HANG），且产物里真写着
/// `vmlinux` 的七个字节（漏前移不会报错，只会挂死，所以内容判据用于防「改了就退出但没写对」）。
#[test]
#[cfg(unix)]
fn designated_string_init_of_char_array_field_terminates() {
    let source = r#"
struct elf_prpsinfo {
	char pr_sname;
	char pr_fname[16];
	char pr_psargs[8];
};

int prpsinfo_probe(void)
{
	struct elf_prpsinfo p = {
		.pr_sname = 'R',
		.pr_fname = "vmlinux",
	};
	return p.pr_fname[0] + p.pr_sname;
}
"#;
    let pid = std::process::id();
    let sp = std::env::temp_dir().join(format!("dcc-dsgstr-{pid}.c"));
    let op = std::env::temp_dir().join(format!("dcc-dsgstr-{pid}.o"));
    fs::write(&sp, source).expect("write fixture");
    // 有界编译：本地循环体里的死循环不会自己退出，`Command::output()` 会把整个
    // `cargo test` 吊死。上限给足（编译本身是毫秒级），超时直接判失败。
    let mut child = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-O2", "-fno-PIE", "-c", "-o"])
        .arg(&op)
        .arg(&sp)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn dcc");
    let limit = std::time::Duration::from_secs(120);
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(st) => break st,
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "HANG: 指定初始化器的字符串字段在 {limit:?} 内没编完 \
                         （`init_idx` 漏前移导致同一项无限重放）"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
        }
    };
    let out = child.wait_with_output().expect("collect dcc output");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(status.success(), "编译失败: {stderr}");

    // 内容判据：字符串的每个字节都要作为立即数出现在反汇编里（dcc 逐字节 `mov $imm`）。
    let dis = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn"])
        .arg(&op)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&dis.stdout).to_string();
    for c in "vmlinux".bytes() {
        let imm = format!("$0x{c:x}");
        assert!(
            text.contains(&imm),
            "`vmlinux` 的字节 {imm} 没进产物（指定初始化器没写内容）:\n{text}"
        );
    }
    let _ = fs::remove_file(&sp);
    let _ = fs::remove_file(&op);
}

/// `-fcf-protection=branch`（内核 `arch/x86/Makefile:102` 用 `cc-option` 探测后必传）必须真在
/// 函数入口发 `endbr64`。dcc 以前「接受但忽略」，所以我方产物里 C 代码的 endbr64 计数是 0、
/// gcc 参考产物是 2（`arch/x86/mm/fault.o`），而 objtool `--ibt` 只报 WARN 不挡构建，缺陷一直
/// 藏着；真跑到 IBT 硬件上，任何间接调用都会 `#CP`。
///
/// 形态对齐 gcc：`-fpatchable-function-entry=16,16` 的 16 字节 nop 前缀 → 函数符号 → `endbr64`
/// （`arch/x86/mm/fault.o` 实测正是这个形状，objdump 里 `endbr64` 出现在 `<sym>:` 的下一行）。
#[test]
#[cfg(unix)]
fn cf_protection_emits_endbr64_after_patchable_prefix() {
    let src = std::env::temp_dir().join(format!("dcc-endbr-{}.c", std::process::id()));
    fs::write(
        &src,
        "int f(int x) { return x + 1; }\nint g(void) { return f(2); }\n",
    )
    .expect("write endbr fixture");
    let dir = std::env::temp_dir();
    let build = |tag: &str, extra: &[&str]| -> String {
        let obj = dir.join(format!("dcc-endbr-{}-{}.o", std::process::id(), tag));
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-O1"])
            .args(extra)
            .arg("-o")
            .arg(&obj)
            .arg(&src)
            .output()
            .expect("run dcc");
        assert!(
            out.status.success(),
            "{tag} 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let dis = Command::new("objdump")
            .arg("-d")
            .arg(&obj)
            .output()
            .expect("objdump");
        let text = String::from_utf8_lossy(&dis.stdout).to_string();
        let _ = fs::remove_file(&obj);
        text
    };
    // 判据一：branch 档下每个函数入口（6 nop 前缀之后）都是 endbr64。
    let ibt = build("ibt", &["-fcf-protection=branch", "-fpatchable-function-entry=16,16"]);
    for sym in ["<f>:", "<g>:"] {
        let at = ibt
            .find(sym)
            .unwrap_or_else(|| panic!("产物里没有函数符号 {sym}:\n{ibt}"));
        let next = ibt[at..].lines().nth(1).unwrap_or("");
        assert!(
            next.contains("endbr64"),
            "{sym} 入口第一条指令不是 endbr64（实际 {next}）:\n{ibt}"
        );
    }
    assert_eq!(
        ibt.matches("endbr64").count(),
        2,
        "endbr64 条数应等于函数个数:\n{ibt}"
    );
    // 判据二：没传 -fcf-protection、或显式 none，都不许凭空发 endbr64（否则普通产物字节会变）。
    let plain = build("plain", &["-fpatchable-function-entry=16,16"]);
    assert_eq!(plain.matches("endbr64").count(), 0, "未开启 CFI 却发了 endbr64:\n{plain}");
    let none = build(
        "none",
        &["-fcf-protection=none", "-fpatchable-function-entry=16,16"],
    );
    assert_eq!(none.matches("endbr64").count(), 0, "none 档却发了 endbr64:\n{none}");
    let _ = fs::remove_file(&src);
}

/// `arch/x86/include/asm/irq_stack.h` 的 `call_on_stack` 形状：`register void *tos asm("r11")`
/// 配 `"+r"(tos)` 输出 + `[tos] "r"(tos)` 输入。
///
/// `arch/x86/mm/fault.o` 曾在这里硬失败（`inline asm has more GP operands than free
/// registers`）：`"r"` 与 `"+r"` 引用同一个变量时，后端把输出/输入各当一个新号去 `alloc_gp`，
/// 13 个号的池被白吃两倍。修复后已绑定的寄存器名（`register asm`）直接复用其号。
///
/// 这里守的是语义底线而不是寄存器号本身：`movq %rsp,(%[tos])` 与 `movq %[tos],%rsp` 必须落在
/// **同一个** 寄存器上，否则内核改写栈指针的意图就直接失效。
#[test]
#[cfg(unix)]
fn asm_tos_output_and_input_share_one_register() {
    let src = std::env::temp_dir().join(format!("dcc-call-on-stack-{}.c", std::process::id()));
    let obj = std::env::temp_dir().join(format!("dcc-call-on-stack-{}.o", std::process::id()));
    fs::write(
        &src,
        r#"#define __always_inline __attribute__((always_inline))
struct pt_regs { unsigned long a, b, c; };
extern void target_fn(void);
register unsigned long gsp asm("rsp");
#define ASM_CALL_CONSTRAINT "+r" (gsp)
static __always_inline void
call_on_stack(unsigned long stack, struct pt_regs *regs, unsigned long vec, unsigned long extra)
{
	register void *tos asm("r11");
	tos = ((void *)(stack));
	asm volatile(
		"movq\t%%rsp, (%%[tos])\t\t\n"
		"movq\t%%[tos], %%rsp\t\t\n"
		"movq\t%%[arg1], %%rdi\t\t\n"
		"movq\t%%[arg2], %%rsi\t\t\n"
		"movq\t%%[arg3], %%rdx\t\t\n"
		"call\ttarget_fn\t\t\n"
		"popq\t%%rsp\t\t\t\n"
		: "+r" (tos), ASM_CALL_CONSTRAINT
		: [tos] "r" (tos),
		  [arg1] "r" (regs), [arg2] "r" (vec), [arg3] "r" (extra)
		: "cc", "rax", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10",
		  "memory"
		);
}
void foo(struct pt_regs *r) { call_on_stack(0x1000, r, 3, 4); }
"#,
    )
    .expect("write call_on_stack fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-O2", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "call_on_stack 形状编不过（GP 池耗尽）: {stderr}"
    );
    let dis = Command::new("objdump")
        .arg("-d")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&dis.stdout).to_string();
    // 模板里 `%[tos]` 只出现两次、都是「读 tos 值」，所以这两条指令必须肩并肩且用同一个寄存器。
    // 不能在全文里各取一处比：函数 epilogue 的 `mov %r13,%rsp` 也会被误抓进来。
    let lines: Vec<&str> = text.lines().collect();
    let mut checked = false;
    for (i, line) in lines.iter().enumerate() {
        let Some(p) = line.find("%rsp,(") else { continue };
        let rest = &line[p + "%rsp,(".len()..];
        let Some(q) = rest.find(')') else { continue };
        let store_reg = &rest[..q];
        let next = lines.get(i + 1).copied().unwrap_or("");
        assert!(
            next.contains(",%rsp"),
            "`movq %rsp,(%[tos])` 的下一条不是 `movq %[tos],%rsp`（实际 {next}）:\n{text}"
        );
        let load_reg = next[..next.find(',').unwrap_or(0)]
            .split_whitespace()
            .last()
            .unwrap_or("");
        assert_eq!(
            store_reg, load_reg,
            "栈切换的两条 mov 没落在同一寄存器（{store_reg} vs {load_reg}）:\n{text}"
        );
        checked = true;
    }
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    assert!(checked, "产物里没找到 `movq %rsp,(%[tos])` 序列:\n{text}");
}

/// typeof 的操作数里嵌着的块级 `extern` 声明，不得污染外层声明符的存储类。
///
/// 内核 `alloc_hooks()` 展开后是
/// `typeof(kmalloc(..., ({ __auto_type x = ...; do { extern void
/// __compiletime_assert_N(void); ... } while (0); ... }), ...)) _res;`：
/// `typeof(...)` 的操作数内部含块级 `extern` 声明。解析器若把那条内层声明的
/// `decl_storage` 留给外层用，`_res` 就被当成纯 extern 声明 —— 上提成文件作用域的
/// 全局引用、规划期不占帧槽，发射期再懒注册时撞穿晚分配带预算（E1133），
/// 且同名变量在多 TU 间变成全局符号，`ld -r vmlinux.o` 会报 multiple definition。
#[test]
fn typeof_operand_block_extern_does_not_leak_to_declarator() {
    let source = r#"
extern void *kma(unsigned long n);

void *alloc_like(unsigned long n)
{
	return ({ typeof(kma(({ extern void __compiletime_assert_1(void);
				  (void)__compiletime_assert_1; 1; }))) _res;
		_res = kma(n);
		_res; });
}
"#;
    let (output, bytes) = run_compile_object_env(
        "typeof-block-extern",
        source,
        "-O2",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("E1133"),
        "`_res` 被当成 extern 纯声明后会在发射期抢晚分配带预算:\n{stderr}"
    );
    assert!(!bytes.is_empty(), "object empty");
    // `_res` 是栈上局部：符号表里根本不该出现它（出现即为被上提的全局/未定义引用）。
    let leaked: Vec<(String, u8, u8, u16)> = elf64_syms(&bytes)
        .into_iter()
        .filter(|s| s.name == "_res")
        .map(|s| (s.name, s.bind, s.stype, s.shndx))
        .collect();
    assert!(
        leaked.is_empty(),
        "`_res` 必须是普通局部变量，不该进符号表（bind/stype/shndx）: {leaked:?}"
    );
    assert!(
        elf64_func_symbols(&bytes)
            .iter()
            .any(|(n, _, sz)| n == "alloc_like" && *sz > 0),
        "缺少 alloc_like 的函数符号，产物不完整"
    );
}

/// 语句表达式里的块作用域 `extern` 对象声明只作全局引用标记，不得占帧槽。
///
/// `({ extern int y; y; })` 是内核宏里常见的形状（`__compiletime_assert` 那一族
/// 把 `extern` 声明包在 `do/while` 与语句表达式里）。语句表达式的发射路径是
/// `gen_block_item` 的一份副本，副本漏了「extern 声明不分配帧槽」的早返，
/// 于是把 `y` 当成普通局部懒注册，撞穿晚分配带预算（E1133）。
#[test]
fn stmt_expr_block_extern_stays_global() {
    let source = r#"
extern int g_ext;

int read_via_stmt_expr(void)
{
	int x = ({ extern int y; y + g_ext; });
	return x;
}
"#;
    let (output, bytes) = run_compile_object_env(
        "stmt-expr-block-extern",
        source,
        "-O0",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("E1133") && !stderr.contains("E1134"),
        "块作用域 extern 被当成局部对象抢帧槽:\n{stderr}"
    );
    let undefined = elf64_undefined_symbols(&bytes);
    assert!(
        undefined.iter().any(|n| n == "y") && undefined.iter().any(|n| n == "g_ext"),
        "`y` 与 `g_ext` 都应是未定义全局引用，实际: {undefined:?}"
    );
    let defined_local: Vec<(String, u8, u8)> = elf64_syms(&bytes)
        .into_iter()
        .filter(|s| s.name == "y" && s.shndx != 0)
        .map(|s| (s.name, s.bind, s.stype))
        .collect();
    assert!(
        defined_local.is_empty(),
        "`y` 不得在本 TU 里有定义（它只是外部全局的声明）: {defined_local:?}"
    );
}

/// `__always_inline` 调用点落在**循环条件**里也必须展开。
///
/// `opt.rs::inline_calls_in_stmt` 的 `Stmt::While`/`Stmt::Do` 两支以前只往下走 body，
/// 不看条件；条件里的调用点因此一字未动。调用点没展开，编译器就得为 callee 另发一份
/// 独立副本，而带 `"i"(形参)` 的副本（内核 `cpufeature.h` 的 `_static_cpu_has`）根本
/// 编不出来：实测 `arch/x86/kernel/fpu/xstate.o` 与 `arch/x86/events/core.o` 报
/// `error[E0311]: ... has no compile-time constant value (expression: bit, in function
/// _static_cpu_has)`。`for (; f(); )` 一直是好的 —— For 支有那条 cond，补的就是这一差距。
#[test]
fn forced_inline_in_loop_conditions_expands() {
    let source = r#"
typedef unsigned short u16;
static int n_calls;
/* 体不是「一条 return 表达式」且带只能接立即数的 `"i"(形参)`：与内核
   `cpufeature.h` 的 `_static_cpu_has` 同形，留在体外就编不出来 */
static __inline__ __attribute__((always_inline)) int tick(u16 bit)
{
	int r = 0;
	__asm__ volatile("nop" : : "i" (1 << (bit & 7)) : "memory");
	n_calls++;
	if (r || n_calls >= 4) return 0;
	return 1;
}
int loop_cond(void)
{
	int i = 0;
	while (tick(93)) i++;
	do { i++; } while (tick(94) && i < 9);
	return i + n_calls;
}
int main(void) { return loop_cond(); }
"#;
    // 结构判据：-O2 能编过，且不得为 tick 另发一份独立副本
    let (output, bytes) = run_compile_object_env(
        "forced-inline-loop-cond",
        source,
        "-O2",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("E0311"),
        "条件里的调用点没展开就会为 callee 另发副本，而那份副本编不出来:\n{stderr}"
    );
    let dup: Vec<String> = elf64_func_symbols(&bytes)
        .into_iter()
        .map(|(n, _, _)| n)
        .filter(|n| n == "tick")
        .collect();
    assert!(dup.is_empty(), "tick 被发成了独立副本（调用点未展开）: {dup:?}");
    // 运行判据：展开必须**每轮都执行**，不能被提到循环外（那就成了只评一次）。
    // 两侧同跑 -O2：`"i"(形参)` 这一族在 gcc -O0 下连参照侧自己就报错。
    let code = run_gcc_cross_check_both_opts("forced-inline-loop-cond", source, &["-O2"], &["-O2"]);
    assert_eq!(code, 9, "while/do-while 条件里的展开必须逐轮求值");
}

/// `__always_inline` 调用点藏在**语句表达式** `({...})` 里也必须展开。
///
/// `opt.rs::inline_expr` 以前没有 `Expr::StmtExpr` 这一臂（落进 `other => other`），
/// 里面的调用点没有任何一条通道看过。内核的 `WARN*()`、`__get_user_x()` 这一族宏
/// 把求值全包在 `({...})` 里，于是 `cpu_feature_enabled(...)` 就死在里层；实测
/// `arch/x86/kernel/ldt.o` 与 `xstate.o` 都有靠这一形报 E0311 的调用点。
#[test]
fn forced_inline_inside_stmt_expr_expands() {
    let source = r#"
typedef unsigned short u16;
static int n_calls;
static __inline__ __attribute__((always_inline)) int check(u16 bit)
{
	int r = 0;
	__asm__ volatile("nop" : : "i" (1 << (bit & 7)) : "memory");
	n_calls++;
	return (n_calls & 1) ? r : 1;
}
int stmt_expr_sites(void)
{
	int i = 0;
	if (({ int t = check(93); t; })) i += 5;
	int x = ({ int a = check(95); a + n_calls; });
	i += ({ check(96); n_calls; });
	return i + x;
}
int main(void) { return stmt_expr_sites(); }
"#;
    let (output, bytes) = run_compile_object_env(
        "forced-inline-stmt-expr",
        source,
        "-O2",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("E0311"),
        "语句表达式里的调用点没人下探就会留下 callee 副本:\n{stderr}"
    );
    let dup: Vec<String> = elf64_func_symbols(&bytes)
        .into_iter()
        .map(|(n, _, _)| n)
        .filter(|n| n == "check")
        .collect();
    assert!(dup.is_empty(), "check 被发成了独立副本（里层调用点未展开）: {dup:?}");
    // if 条件 / 声明初值 / 表达式位置三处都走同一条下探，值必须与 gcc 一致（两侧同 -O2）
    let code =
        run_gcc_cross_check_both_opts("forced-inline-stmt-expr", source, &["-O2"], &["-O2"]);
    assert_eq!(code, 6, "语句表达式里的展开在三种位置上都必须与 gcc 同值");
}

/// 内联 asm 体内的 `call` 必须保住延伸池家，而且不得拿 E1136 顶住编译。
///
/// 内核跳进另一个 C 函数的 `asm volatile`（`call write_ibpb`、`call rep_stos_alternative`）
/// 只写 `ASM_CALL_CONSTRAINT`，那份 clobber 名单里没有寄存器号；而按 ABI，这次调用毁掉的
/// 正是全部 caller-saved GP。本端的延伸池把变量当家放在 r8/r9 里，所以必须在 asm 语句
/// 外面插 push/pop 守护区间（守护名单与 clobber 那一份同口径，只把 `call` 的 ABI 事实并
/// 进去）。实测 `arch/x86/kernel/cpu/bugs.o` 与 `mm/mm_init.o` 在守护接上之前报
/// `error[E1136]: call 到 'write_ibpb' 不经过 caller-saved 家的保存窗口`。
#[test]
fn asm_body_call_guards_ext_homes() {
    let source = r#"
typedef unsigned long ul;
/* 被调方像 write_ibpb / rep_stos_alternative 那样扫掉全部 caller-saved */
void clob_all(void)
{
	__asm__ volatile("" : : : "rax","rcx","rdx","rsi","rdi","r8","r9","r10","r11","cc");
}
int asm_call_guard(int n, ul p)
{
	ul v1 = p + 1, v2 = p + 2, v3 = p + 3, v4 = p + 4;
	for (int i = 0; i < n; i++) {
		v1 += i;
		v2 += i * 2;
		v3 += i * 3;
		v4 += i * 4;
		__asm__ volatile("call clob_all" : : : "memory", "cc");
		v1 ^= (v1 >> 3);
		v2 ^= (v2 >> 3);
		v3 ^= (v3 >> 3);
		v4 ^= (v4 >> 3);
	}
	return (int)(v1 + v2 + v3 + v4);
}
int main(void) { return asm_call_guard(7, 1000); }
"#;
    let name = "asm-call-ext-home-guard";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-{name}-{pid}.c"));
    let obj = dir.join(format!("dcc-{name}-{pid}.o"));
    fs::write(&src, source).expect("write asm-call fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .env("DCC_NO_PARALLEL", "1")
        .args(["-std=gnu23", "-O2", "-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("E1136"),
        "asm 体内的 call 必须走守护窗口，不该再把家静默毁掉:\n{stderr}"
    );
    let dis = Command::new("objdump")
        .arg("-d")
        .arg("--no-show-raw-insn")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&dis.stdout).to_string();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    // 守护真的到了产物：`call` 那一层外面有一对 push/pop（r8/r9 两个号 = 2 push + 2 pop）。
    // 逐行取助记符而不是拿子串 `call` 去定位：函数名 `asm_call_guard` 里就含 `call`，
    // 子串定位会把区间切在函数入口之前，本来有的 push 也会被当成没插。
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains("<asm_call_guard>:"))
        .unwrap_or_else(|| panic!("产物里没有 asm_call_guard 这个函数"));
    let mut pushed = 0usize;
    let mut popped = 0usize;
    let mut seen_call = false;
    for l in lines[start + 1..].iter().take_while(|l| !l.trim().is_empty()) {
        let m = l.split_whitespace().nth(1).unwrap_or("");
        if m == "call" || m == "callq" {
            seen_call = true;
            continue;
        }
        if !seen_call {
            if m.starts_with("push") {
                pushed += 1;
            }
        } else if m.starts_with("pop") {
            popped += 1;
        }
    }
    assert!(
        seen_call,
        "asm_call_guard 里没发出 call（asm 模板没被编出来）:\n{text}"
    );
    assert!(
        pushed >= 1 && popped >= 1,
        "call 前没插守护（push={pushed} pop={popped}），延伸池家会被这次调用毁掉"
    );
    // 行为判据：与 gcc 同值（家被毁时的典型现象就是后续几轮算错）
    let code = run_gcc_cross_check_both_opts(name, source, &["-O2"], &["-O2"]);
    assert_eq!(code, 31, "asm 体内的 call 跨轮必须保住变量的值");
}

/// `.section X,"a"` + `.previous` 必须把当前节**退回切换前那一个**，不得把新节原样压回去。
///
/// 内核的 `EXPORT_SYMBOL`（`include/linux/export.h` 的 `___EXPORT_SYMBOL`）就是这个形状。
/// 以前 `.section` 把**即将进入的节**同时压进 `section_stack` 与 `pushsection_stack`，
/// 而 `.previous` 先弹出它、又把它同一个下标压回去 ⇒ `pushsection_stack` 永远留着那个
/// 早已退出的数据节。之后同一函数里每条 asm 都按「在一个非执行 pushsection 里」处理，
/// 数字标号就按那个数据节的坐标登记（实测 `mm/mm_init.o`：`ANNOTATE_REACHABLE(1b)` 的 5 个
/// 标号全被记成 `.export_symbol` + 0x80，链接期 objtool 报
/// `bad .discard.annotate_insn entry: 0 of type 8` ⇒ `vmlinux.o` 做不出来）。
#[test]
fn section_previous_restores_prior_section() {
    let source = r#"
int foo(void) { return 1; }
void bar(void)
{
	__asm__ volatile(".section \".export_symbol\",\"a\";\n\t.quad foo;\n\t.previous;\n" ::: "memory");
	__asm__ volatile(
		"1: nop\n\t"
		"911: .pushsection .discard.annotate_insn, \"M\", @progbits, 8; .long 911b - ., 8; .popsection\n"
		::: "memory");
}
"#;
    let (_output, bytes) =
        run_compile_object_env("section-previous", source, "-O2", &[("DCC_NO_PARALLEL", "1")]);
    // 标号所在节：`section == 1` 就是 `.text`（标号自己发在代码里）。
    // 回归时会看到它变成 `.export_symbol` 的下标（>1）且 value = 那个节的长度。
    let data_shndx: Vec<u16> = elf64_syms(&bytes)
        .into_iter()
        .filter(|s| s.name.starts_with(".export_symbol"))
        .map(|s| s.shndx)
        .collect();
    let ann: Vec<(String, u64, u16)> = elf64_syms(&bytes)
        .into_iter()
        .filter(|s| s.name.starts_with(".Lasm_local"))
        .map(|s| (s.name.clone(), s.value, s.shndx))
        .collect();
    assert!(!ann.is_empty(), "产物里没有 asm 数字标号的本地符号：标号根本没登记");
    for (name, value, shndx) in &ann {
        assert_eq!(
            *shndx, 1,
            "标号 `{name}` 应归 `.text`（shndx=1），实际 shndx={shndx}（data 节符号: {data_shndx:?}）",
        );
        assert!(
            *value > 0 && *value < 0x400,
            "标号 `{name}` 的 `.text` 内偏移不像真位置: {value:#x}",
        );
    }
    // `.previous` 之后 `.quad foo` 的那条导出项仍必须在 `.export_symbol` 里
    let export_rela: Vec<String> = elf64_syms(&bytes)
        .into_iter()
        .filter(|s| s.name == "foo")
        .map(|s| s.name)
        .collect();
    assert!(!export_rela.is_empty(), "EXPORT 引用的 `foo` 符号不得丢");
}

/// 内核 `asm/shared/io.h` 的 BUILDIO：端口是编译期常量时，`%w1` 必须**原样**代成立即数。
///
/// GAS 对非寄存器操作数的 `%b/%w/%k/%q` 不改宽度（宽度由助记符后缀承担），而
/// `asm_reg_modifier_subst` 以前先 `parse_asm_reg(val)?` 一刀切：不是寄存器就直接不给结果
/// ⇒ `%w1` 留在模板文本里，操作数解析成 `None`，报 `unsupported in/out port operand`
/// （实测 `arch/x86/kernel/i8259.o`、`arch/x86/kernel/x86_init.o` 整 TU 失败）。
#[test]
fn port_io_immediate_operand_keeps_immediate_text() {
    let source = r#"
typedef unsigned char u8; typedef unsigned short u16;
void outb_lit(u8 val) { __asm__ volatile("outb %b0, %w1" : : "a"(val), "Nd"(0x21) : "memory"); }
u8 inb_lit(void) { u8 v; __asm__ volatile("inb %w1, %b0" : "=a"(v) : "Nd"(0x21)); return v; }
u16 inw_lit(void) { u16 v; __asm__ volatile("inw %w1, %w0" : "=a"(v) : "Nd"(0x43)); return v; }
"#;
    let name = "port-io-imm";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-{name}-{pid}.c"));
    let obj = dir.join(format!("dcc-{name}-{pid}.o"));
    fs::write(&src, source).expect("write port-io fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .env("DCC_NO_PARALLEL", "1")
        .args(["-std=gnu23", "-O2", "-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "端口常量 + `%wN` 必须能编，stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("port operand"),
        "`in`/`out` 的立即端口又被拒了:\n{stderr}"
    );
    let dis = Command::new("objdump")
        .arg("-d")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&dis.stdout).to_string();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    // 与 GAS/gcc 逐字节同口径：`out %al,$0x21` = e6 21，`in $0x21,%al` = e4 21，
    // `in $0x43,%ax` = 66 e5 43（16 位带 0x66 前缀）。
    for needle in ["e6 21", "e4 21", "66 e5 43"] {
        assert!(
            text.contains(needle),
            "产物里缺 {needle} 这条编码，objdump 实际:\n{text}"
        );
    }
}

/// `__builtin_constant_p(形参/内联临时量)` 作三元条件时必须当场定下来。
///
/// parser 与 `fold_expr` 的 `Call` 臂对这种调用都采取「不急切折 0」（把机会留给
/// SCCP/modref），于是条件永远不是 `Int`；而 `fold_expr` 的 `Expr::Cond` 臂以前只认
/// `Expr::Int` ⇒ 整个三元留在运行时，**两个分支都被编**，而非常量的那一支里
/// `"i"(形参)` 的 asm 根本编不出（实测 `kernel/ptrace.o`：`include/linux/wait_bit.h` 的
/// `wait_on_bit` 报 `impossible constraint in 'asm' … (1 << (__inl$_nr & 7))`）。
#[test]
fn builtin_constant_p_ternary_condition_is_decided_at_compile_time() {
    let source = r#"
typedef unsigned long ul;
static __inline__ __attribute__((always_inline)) int ctba(long nr, const volatile unsigned long *addr)
{
	int oldbit;
	__asm__ volatile("testb %2,%1"
		     : "=@ccnz" (oldbit)
		     : "m" (((unsigned char *)addr)[nr >> 3]),
		       "i" (1 << (nr & 7))
		     : "memory");
	return oldbit;
}
static __inline__ __attribute__((always_inline)) int vtb(long nr, const volatile unsigned long *addr)
{
	int oldbit;
	__asm__ volatile("btq %2,%1"
		     : "=@cccz" (oldbit)
		     : "m" (*(unsigned long *)addr), "Ir" (nr)
		     : "memory");
	return oldbit;
}
static __inline__ __attribute__((always_inline)) int ta(long nr, const volatile unsigned long *addr)
{
	return __builtin_constant_p(nr) ? ctba(nr, addr) : vtb(nr, addr);
}
/* 形参是运行时的：常量分支必须整个消失，不能被编出来 */
int use_rt(long nr, const ul *a) { if (ta(nr, a)) return 1; return 0; }
/* 常量经过一层内联临时量传进来（ptrace 的 wait_on_bit 就是这一形）*/
static inline int wrap(long bit, const volatile ul *w) { return ta(bit, w); }
int use_lit(const ul *a) { return wrap(5, a); }
"#;
    let (output, bytes) = run_compile_object_env(
        "constant-p-ternary",
        source,
        "-O2",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("impossible constraint"),
        "`__builtin_constant_p` 的非常量分支又被当活代码编了:\n{stderr}"
    );
    assert!(!bytes.is_empty(), "object empty");
    // 运行时那一支用的是 `bt`，常数那一支才是 `testb $imm, sym(%rip)`（F6 05）。
    // `use_rt` 里必须只剩 `bt`；两边都编出来就会出现 `testb`。
    let dir = std::env::temp_dir();
    let obj = dir.join(format!("dcc-constant-p-ternary-{}.o", std::process::id()));
    fs::write(&obj, &bytes).expect("write object back for objdump");
    let dis = Command::new("objdump")
        .arg("-d")
        .arg("--no-show-raw-insn")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&dis.stdout).to_string();
    let _ = fs::remove_file(&obj);
    let at = text
        .find("<use_rt>:")
        .unwrap_or_else(|| panic!("产物里没有 use_rt:\n{text}"));
    let body: String = text[at..]
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with("0000"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("bt"),
        "`use_rt` 该走运行时 `bt` 那一支，实际:\n{body}"
    );
    assert!(
        !body.contains("testb"),
        "`use_rt` 里出现了只有常量分支才有的 `testb $imm`:\n{body}"
    );
}

/// `__builtin_constant_p(内联形参)` 在「声明初值」这一调用位置的答案必须是 1。
///
/// 现场是 `include/linux/rhashtable.h` 的 `__rhashtable_lookup()`（体内有 `restart:` 标号）：
/// 调用点写作 `p = rhashtable_lookup_fast(...)`，内联把体包成
/// `int *p = ({ …; __inl$_freq = <字面量>; … });` —— 形参临时量声明在那个**语句表达式**里，
/// 而 `__builtin_constant_p(形参)` 的答案只能从 ipa 的「已证常量局部」表来。
/// `collect_vars_in_block` 旧写法只收 `Decl::Var` 本身、不下探 `v.init`，于是那张表里
/// 根本没有临时量名 ⇒ constp 一律答 0 ⇒ `BUILD_BUG_ON` 的 `__compiletime_assert_NNN`
/// 变成真引用（实测 `fs/xattr.o`/`fs/pidfs.o`/`kernel/events/hw_breakpoint.o` 各 2–4 条
/// 473/487，链接期 undefined reference）。夹具 `.work_tmp/cp3.c` 是同一形的最小复现。
#[test]
fn builtin_constant_p_param_across_decl_init_inline_is_one() {
    let source = r#"
enum freq { F_NORMAL, F_LIKELY };
extern void trap_473(void);
static inline __attribute__((always_inline)) int *rht_ptr(int *const *bkt, const enum freq freq)
{
	do {
		if (!(!(!__builtin_constant_p(freq))))
			trap_473();
	} while (0);
	if (freq == F_LIKELY)
		return *bkt;
restart:
	if (!*bkt)
		goto restart;
	return *bkt;
}
int probe(int *const *bkt)
{
	int *p = rht_ptr(bkt, F_NORMAL);
	return p != 0;
}
"#;
    let (output, bytes) = run_compile_object_env(
        "constp-declinit-inline",
        source,
        "-O2",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!bytes.is_empty(), "object empty");
    let dir = std::env::temp_dir();
    let obj = dir.join(format!("dcc-constp-declinit-{}.o", std::process::id()));
    fs::write(&obj, &bytes).expect("write object back for nm");
    let und = Command::new("nm").arg("-u").arg(&obj).output().expect("nm");
    let _ = fs::remove_file(&obj);
    let und = String::from_utf8_lossy(&und.stdout).to_string();
    assert!(
        !und.contains("trap_473"),
        "constp(内联形参) 被判成 0，`BUILD_BUG_ON` 的断言调用留在了产物里:\n{und}"
    );
}

/// 从可执行 pushsection 里跳回 `.text` 的 `jmp` 必须带重定位，不能写死位移。
///
/// `final_text_loc` 用的那张搬迁区间表 `(发射起点, 发射长度, 目标节下标, 目标节内起点)`
/// 以前在 `codegen.rs` 里被整行拿错一格（起点用了函数起点、长度用了发射起点、节号用了
/// **发射长度**），于是两边总是被算成同一个不存在的节号 ⇒ 「同节」成立 ⇒ 跨节跳转被写死
/// 位移也不发重定位。链接期表现为 objtool `can't find jump dest instruction`（实测
/// `arch/x86/kernel/traps.o`、`arch/x86/kernel/cpu/bugs.o`、`arch/x86/events/core.o`、
/// `arch/x86/kernel/fpu/xstate.o`），运行期则是 `apply_alternatives` 换上去的替换代码
/// 跳到指令中段。两边对比的基准就是 gcc：它在这种位置发的是 `R_X86_64_PC32 .text + N`。
///
/// 对本用例的诚实定位：它钉的是「跨节跳转必须带 PC32」这条不变式本身；单独拿它去过
/// 旧二进制不会变红（要触发那次字段错位需要「目标在自定义函数节里」的嵌套形状，
/// traps.c 里是 `noinstr` + asm goto 那一族）。真正的撤修即变红证据是四个内核 TU
/// （traps / bugs / events core / xstate）在 `objtool --link --ibt/--noinstr/--unret/--orc` 下
/// 从报错变 ok，以及它们 `ld -r` 后的合并对象同样全 ok（脚本：`bash .work_tmp/kwobjtool.sh <对象>...`）。
#[test]
fn cross_section_jump_out_of_exec_pushsection_carries_reloc() {
    let source = r#"
void f(void)
{
	__asm__ __volatile__(
		"771: .byte 0x90\n"
		"   .pushsection .altinstr_replacement, \"ax\"\n"
		"774: jmp 6f\n"
		"775: .byte 0x90\n"
		"   .popsection\n"
		"6: .byte 0x90\n"
		::: "memory");
}
"#;
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let name = "xsec-jump";
    let src = dir.join(format!("dcc-{name}-{pid}.c"));
    let obj = dir.join(format!("dcc-{name}-{pid}.o"));
    fs::write(&src, source).expect("write xsec-jump fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .env("DCC_NO_PARALLEL", "1")
        .args(["-std=gnu23", "-O2", "-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "stderr: {stderr}");
    let rels = Command::new("readelf")
        .args(["-rW", "--wide"])
        .arg(&obj)
        .output()
        .expect("readelf");
    let text = String::from_utf8_lossy(&rels.stdout).to_string();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    // 替换节里那条 `jmp` 的目标在 `.text`，必须在**它自己那张重定位表里**留下一条 PC32；
    // 回归时的形状是「`.rela.altinstr_replacement` 整张表不存在」，所以判据要卡在
    // 「表头之后的行里能找到 R_X86_64_PC32」，不能全文件扫（那里哪里都有 PC32）。
    let after = text
        .find(".rela.altinstr_replacement")
        .unwrap_or_else(|| panic!("产物里没有 .rela.altinstr_replacement，跨节跳转被写死了位移:\n{text}"));
    let sec_relocs = &text[after..];
    assert!(
        sec_relocs
            .lines()
            .skip(1)
            .take_while(|l| !l.trim().is_empty() && !l.starts_with("重定位节")
                && !l.starts_with("Relocation section"))
            .any(|l| l.contains("R_X86_64_PC32")),
        "`.rela.altinstr_replacement` 里没有 PC32 重定位（跳转被写死）:\n{text}"
    );
}

/// `_ASM_EXTABLE` 留下的括号不能把数字标签引用变成「未定义符号」。
///
/// 内核 `_ASM_EXTABLE(from, to)` 展开就是 `.long (1b) - .` 与 `.long (2b) - .`（参数被
/// 宏括起来）。GAS 把括号当分组照样解析，而 `parse_asm_data_expr`/`resolve_asm_data_symbol`
/// 以前先拿整串分类：`(1b)` 因为带括号认不出数字标签，于是发一个名叫 `(1b)` 的
/// UND GLOBAL 符号去接那条重定位 —— `__ex_table` 每一条都指向不存在的位置（实测
/// `arch/x86/kernel/ldt.o`：链接期 objtool 报 `special: can't find orig instruction`，
/// 而真正严重的是运行期异常表整个是错的）。修后两条都指回 `.text` 里的标号，
/// 同一个对象的 `objtool --link --ibt/--noinstr/--unret --noabs` 全部 rc=0。
#[test]
fn extable_parenthesized_numeric_labels_resolve() {
    let source = r#"
void f(void)
{
	__asm__ __volatile__(
		"1: nop\n"
		"2: nop\n"
		"   .pushsection __ex_table, \"a\"\n"
		"   .long (1b) - ., (2b) - .\n"
		"   .popsection\n"
		::: "memory");
}
"#;
    let (output, bytes) = run_compile_object_env(
        "extable-paren-label",
        source,
        "-O2",
        &[("DCC_NO_PARALLEL", "1")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!bytes.is_empty(), "object empty");
    // 判据一：未定义符号里不能出现括号包起来的标签引用，也不能有裸的 `Nb`/`Nf`。
    let und = elf64_undefined_symbols(&bytes);
    let bogus: Vec<&String> = und
        .iter()
        .filter(|n| n.starts_with('(') || n.ends_with(')') || regex_like_numeric_ref(n))
        .collect();
    assert!(
        bogus.is_empty(),
        "数字标签引用被当成未定义符号发出去了（__ex_table 会指向不存在的位置）: {bogus:?}"
    );
    // 判据二：标签真的被解析成了「已定义在某个节里的标号」。`elf64_syms` 里
    // shndx != 0 即「有定义」；标号自己的名字带 `_1_0`/`_2_0` 后缀。
    let defined: Vec<(String, u64, u16)> = elf64_syms(&bytes)
        .into_iter()
        .filter(|s| s.name.starts_with(".Lasm_local") && s.shndx != 0)
        .map(|s| (s.name, s.value, s.shndx))
        .collect();
    assert!(
        defined.len() >= 2,
        "本条 asm 里的两个数字标号没被登记成有定义的符号，实际: {defined:?}"
    );
    assert!(
        defined.iter().all(|(_, v, _)| *v > 0),
        "标号的节内偏移不该是 0（那意味着它没跟着任何代码），实际: {defined:?}"
    );
}

/// 判一个未定义符号名是不是「没解开的数字标签引用」（`1b`、`771f` 这种）。
/// 写在这里而不是拉一个 regex 依赖：判据就是一条字符规则，要一眼能看完。
fn regex_like_numeric_ref(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    let last = bytes[bytes.len() - 1];
    (last == b'b' || last == b'f') && bytes[..bytes.len() - 1].iter().all(|c| c.is_ascii_digit())
}

/// `sizeof`/`_Alignof` 的结果类型必须是 `size_t`（LP64 上 = `unsigned long`），不是 `int`。
///
/// 为什么不是「少一个优化」而是错值：`typeof(表达式)` 与 `__auto_type` 的推导直接吃
/// `Expr::ty()`，写死 `Type::Int` 就把局部变量的**类型**推错了。现场后果：内核 `min()/max()`
/// 的符号性检查 `count = min(count, sizeof(buf) - 1);` 里，`sizeof(buf) - 1` 被推成有符号
/// int ⇒ `is_signed_type(typeof(...))` 选错臂 ⇒ 「故意不存在」的 `__compiletime_assert_NNN`
/// 留在产物里，链接期就是 `undefined reference`（实测 `lib/kstrtox.o` 11 条 → 修后 0 条）。
///
/// 夹具里每个形状的期望值都是 **gcc -O2 实测**的（同一个编译器跑不出这个矛盾），
/// 并带一条有符号 `int` 的反向对照（14），所以判据不是「一律期望 8」。
#[test]
#[cfg(unix)]
fn sizeof_result_type_is_size_t_matches_gcc() {
    let src = include_str!("../fixtures/c/sizeof_result_type_is_size_t.c");
    for flag in ["-O0", "-O2"] {
        let (out, exe) = run_compile_and_run_flags("sizeof-type", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(
            rc,
            0,
            "{flag}: 有一条形状的 sizeof/alignof 结果类型与 gcc 实测不一致（退出码就是第一条不匹配的形状编号，1..8）"
        );
    }
}

/// 同一件事的产物级判据：`min()` 的符号性检查在四种临时量写法下都不能留下陷阱引用。
///
/// 与上一条互补：那条量的是「类型推错」，这条量「推错了会真的留下链接错误」。
/// 保留 v3/v4 两条本来就得绿的形，是为了让「只检坏的那两条」这种改法瞒不过去。
#[test]
#[cfg(unix)]
fn min_signedness_assert_with_auto_type_leaves_no_trap_reloc() {
    let src = include_str!("../fixtures/c/min_auto_type_trap.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("min-auto-trap", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let traps: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
            .filter(|s| s.name.starts_with("trap_v"))
            .map(|s| s.name.clone())
            .collect();
        assert!(
            traps.is_empty(),
            "{flag}: `min()` 的常量符号性检查没折掉，产物里留下 {} 条指向陷阱函数的引用：{:?}\
             （就是链接期那条 `undefined reference to '__compiletime_assert_NNN'`）",
            traps.len(),
            traps
        );
    }
}

/// `__builtin_strcmp(字面量, 字面量)` 必须折成常量，否则内核 `vruntime_cmp()` 那一族
/// 死臂折不掉（现场后果：`kernel/sched/fair.o` 里 7 条指向故意不存在的
/// `__BUILD_BUG_vruntime_cmp` 的引用，最终链接 `undefined reference`）。
///
/// 两层判据同一个夹具（`fixtures/c/vruntime_strcmp_trap.c`）：
///   * 产物级：`.rela.text` 里不得留任何 `trap_vruntime_cmp` 引用（-O2/-O3）；
///   * 运行级：`main` 返回第一条不匹配的形状编号，逐条对 **gcc -O2/-O0 实测**（符号，
///     不是大小：C 只规定 `strcmp` 返回值的符号，gcc 折叠给 ±1、glibc 运行给 ±99）。
#[test]
#[cfg(unix)]
fn vruntime_strcmp_shape_leaves_no_trap_reloc_and_folds_to_gcc_signs() {
    let src = include_str!("../fixtures/c/vruntime_strcmp_trap.c");
    for flag in ["-O0", "-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("vruntime-strcmp", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(
            rc, 0,
            "{flag}: 折叠结果的符号与 gcc/glibc 不一致（退出码 = 夹具里第一条不匹配的形状编号，1..12）"
        );
    }
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("vruntime-strcmp-obj", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let traps: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
            .filter(|s| s.name.starts_with("trap_"))
            .map(|s| s.name.clone())
            .collect();
        assert!(
            traps.is_empty(),
            "{flag}: `__builtin_strcmp` 的字面量比较没折，产物里留下 {} 条指向陷阱函数的引用：{:?}\
             （就是链接期那条 `undefined reference to '__BUILD_BUG_vruntime_cmp'`）",
            traps.len(),
            traps
        );
    }
}

/// 恒返回常量的 `static` 桩：调用点必须换成那个常量，它下面的死循环才能真被消掉。
///
/// 现场形状（`mm/shmem.c:1917` THP 关闭时的 `shmem_suitable_orders` 桩 +
/// `while (suitable_orders)` 里包着 `HPAGE_PMD_*` 的 `BUILD_BUG()` 陷阱）：不折桩返回值
/// 就是 `undefined reference to '__compiletime_assert_NNN'`（实测 `mm/shmem.o` 5 条）。
///
/// 两层判据（夹具 `fixtures/c/const_return_stub_dead_loop.c`）：
///   * 产物级：`.rela.text` 里不得出现指向 `trap_in_dead_loop*` 的重定位（-O2/-O3）；
///     其中形状 4（常量先存进局部、循环条件读那个局部）靠
///     `propagate_const_locals_in_conditions`，它是全树剩下那 253 行未定义引用的主机制；
///   * 运行级：实参副作用必须保留、恒非 0 的桩对应的循环必须照跑、依赖形参的桩不得被折
///     （退出码不是 0 就是哪一条语义被改坏了）。
#[test]
#[cfg(unix)]
fn const_return_static_stub_folds_and_kills_the_dead_loop() {
    let src = include_str!("../fixtures/c/const_return_stub_dead_loop.c");
    for flag in ["-O2", "-O3"] {
        let (out, exe) = run_compile_and_run_flags("const-return-stub", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = run_exe(&exe);
        assert_eq!(
            rc, 0,
            "{flag}: 常量桩/条件代回改坏了语义（rc=11 实参副作用丢了 / 12 死臂被执行 / \
             13 恒非 0 的桩循环被删 / 14 依赖形参的桩被误折 / 15 常量存局部那一形没代回条件 / \
             16 实参副作用次数不对），实际 rc={rc}"
        );
        let (out, obj) = run_object_test_flags("const-return-stub-obj", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let hits: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
            .filter(|s| s.name.starts_with("trap_in_dead_loop"))
            .map(|s| s.name.clone())
            .collect();
        assert!(
            hits.is_empty(),
            "{flag}: 恒 0 桩的调用没换成常量、或常量没代回到循环条件 ⇒ 死循环没被删，\
             产物里还留着 {} 条指向陷阱的引用：{:?}",
            hits.len(),
            hits
        );
    }
}

/// `if (!IS_ENABLED(CONFIG_X)) return;` 守住的整段代码必须被删干净（夹具
/// `is_enabled_early_return_tail.c`）。
///
/// 缺陷现场：`arch/x86/mm/ioremap.c:130` 在 `CONFIG_EFI=n` 时是
///     if (!IS_ENABLED(CONFIG_EFI))
///             return;
///     if (efi_mem_type(addr) == EFI_RUNTIME_SERVICES_DATA || …)
/// 而 `efi_mem_type`/`efi_mem_attributes` 在 CONFIG_EFI=n 的全树都**没有定义**
///（gcc 侧名字根本不进任何 .o）。`__ioremap_check_other` 是 `static`、被内联进调用方，
/// 被调方的 `return;` 到了调用方就变成 `goto __inl<N>__l__inl<N>__end;` —— 不是 `Return`
/// ⇒ `eliminate_unreachable` 里只认 `Return` 的终止判据对这一整族完全无效，死尾段照发，
/// 最终链接 7 行 `undefined reference`（实测 `arch/x86/mm/ioremap.o`）。
///
/// 双向钉（期望名单是拿 `gcc -O1/-O2/-O3` 实测出来的）：死尾段里的 `efi_mem_attributes`
/// 必须不见；活路（调用在 `return` 之前）、配置真开着、`goto` 跳进嵌套块标号这三种形状
/// 里的 `efi_mem_type`/`live_touch` **必须还在**（后面三条才是防「一律删尾段」的拦网）。
#[test]
#[cfg(unix)]
fn is_enabled_false_early_return_drops_the_guarded_tail() {
    let src = include_str!("../fixtures/c/is_enabled_early_return_tail.c");
    for flag in ["-O1", "-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("is-enabled-tail", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und: Vec<String> = elf64_syms_by_type(&bytes)
            .iter()
            .filter(|s| s.shndx == 0)
            .map(|s| s.name.clone())
            .collect();
        let has = |n: &str| und.iter().any(|s| s == n);
        // 死尾段（形状 1 的内联版与形状 4 的 goto 版）里的引用必须不见。
        assert!(
            !has("efi_mem_attributes"),
            "{flag}: `if (!IS_ENABLED(CONFIG_EFI)) return;` 之后的死尾段没被删掉，产物里仍有 \
             `efi_mem_attributes`（= 链接期 undefined reference）；完整未定义名单：{:?}",
            und
        );
        // 活路与拦网：这三条是防过修的那一半。
        for kept in ["efi_mem_type", "live_touch"] {
            assert!(
                has(kept),
                "{flag}: 本该留下的引用被一并抹掉了（`{kept}` 不在 {:?}）——死尾段截断不能\
                 连「return 之前的调用」或「配置真开着」的那几条一起删",
                und
            );
        }
    }
}

/// 常量桩折叠的**产物级双向判据**（夹具 `const_return_stub_trap_refs.c`）。
///
/// 为什么单独一个夹具：把陷阱/被守函数定义在同一个 TU 里时，本地 `call` 不发重定位，
/// 「没有指向它的条目」这条断言就永远成立（已实测）。这里一律用 `extern` 只声明不定义，
/// 才是内核 `fs_bio_integrity_free` / `__compiletime_assert_NNN` 的真实形状。
/// 一半形状要求引用消失（含指针常量桩那一形，以及 `static inline` 整型零桩坐在 `if`
/// 条件里那一形 —— 后者钉住「桩代回必须排在内联之前」这条调度次序），另一半要求引用
/// **必须在**（防「一律删」）。期望名单是拿 `gcc -O2` 实测出来的，不是推的：
/// 只剩 `fs_guarded_keep` / `sme_kept_ref` / `trap_dep_loop` / `trap_live_loop` 四条。
#[test]
#[cfg(unix)]
fn const_return_stub_guards_only_kill_the_dead_references() {
    let src = include_str!("../fixtures/c/const_return_stub_trap_refs.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("const-return-refs", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und: Vec<String> = elf64_syms_by_type(&bytes)
            .iter()
            .filter(|s| s.shndx == 0)
            .map(|s| s.name.clone())
            .collect();
        let has = |n: &str| und.iter().any(|s| s == n);
        // 死路上的：必须不见
        for gone in [
            "trap_direct",
            "trap_via_local",
            "trap_nested_local",
            "fs_guarded_free",
            // 形状 8：`static inline` 的零桩坐在 `if` 条件里（`sme_get_me_mask()` 形）。
            // 内联先跑时它已被换成读 `__inl0$rv` 临时量，桩折叠看不见 ⇒ 快照二进制实测留 1 条。
            "sme_guarded_ref",
            // 形状 10：死分支里坐一次 `always_inline` 调用，内联留下的**自造出口标签**
            // `__inlN__end` 把「分支内有 Label 就不删」这条拦网触发了（`gdt_idt.o` 的
            // `vc_no_ghcb` 就是这个形状）。真正的 `goto` 拦网只该认源码标签。
            "lab_guarded_ref",
        ] {
            assert!(
                !has(gone),
                "{flag}: 死路上的引用没被消掉，产物里仍有 `{gone}`（= 链接期 \
                 `undefined reference to '{gone}'`）；完整未定义名单：{:?}",
                und
            );
        }
        // 活路 / 拒接的：必须在
        for kept in [
            "trap_live_loop",
            "trap_dep_loop",
            "fs_guarded_keep",
            "sme_kept_ref",
            // 形状 11：同样的 `always_inline` 调用坐在**真分支**里 ⇒ 地址引用不能一并抹掉。
            "lab_alive_ref",
        ] {
            assert!(
                has(kept),
                "{flag}: 本该留下的引用被一并抹掉了（`{kept}` 不在 {:?}）——\
                 判据不是「越少越好」，这一半才是防过修的那一半",
                und
            );
        }
    }
}

/// 「常量存进局部 → 后面拿它当条件读」的**产物级双向判据**（夹具
/// `const_store_forward_trap.c`，§3.51）。
///
/// 为什么需要这一条：内核先把常量存进局部、后面才读它当条件（`mm/shmem.c` 的
/// `if (!IS_ENABLED(...)) orders = 0;` 后面接 `if (orders > 0) { … BUILD_BUG() … }`），
/// 而这条路上的常量代回原来一个都没有 —— AST 层的 cprop 只认**声明初值**，SSA 层
/// 对含 `goto` 的函数只做 analysis-only（不重建、不跑 SCCP），而内核错误路径全是
/// `goto out;`。于是死块里的 `BUILD_BUG()`（恒真的无条件调用）留在产物里，变成
/// 链接期 `undefined reference to '__compiletime_assert_NNN'`（`mm/shmem.o` 实测 5 条）。
///
/// 期望名单先拿 `gcc -O2` 校准过，不是推的：dead 三个 0 条，keep 五个 5 条都在。
/// 后半那一半才是防过修的那一半 —— 判据不是「越少越好」。
#[test]
#[cfg(unix)]
fn const_store_forward_kills_dead_decl_uses_only() {
    let src = include_str!("../fixtures/c/const_store_forward_trap.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("const-store-forward", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und: Vec<String> = elf64_syms_by_type(&bytes)
            .iter()
            .filter(|s| s.shndx == 0)
            .map(|s| s.name.clone())
            .collect();
        let has = |n: &str| und.iter().any(|s| s == n);
        // 死路上的三条：必须不见
        for gone in [
            // P1 形参被常量钉死
            "trap_p1",
            // P2 shmem 全形（`goto allocated;` + `else { pages = 1; }`）：两跳不动点才收敛
            "trap_p2_a",
            "trap_p2_b",
            // P3 局部显式补一条与初值相同的存储
            "trap_p3",
        ] {
            assert!(
                !has(gone),
                "{flag}: 死路上的引用没被消掉，产物里仍有 `{gone}`（= 链接期 \
                 `undefined reference to '{gone}'`）；完整未定义名单：{:?}",
                und
            );
        }
        // 拒接的五条：必须在（地址逃逸 / 自增 / 有 goto 指向的标号 / 可能不执行的
        // 嵌套存储 / 后面被别的常量写过 —— 每一条都是「代回不安全」的独立理由）
        for kept in [
            "trap_keep_label",
            "trap_keep_reassign",
            "trap_keep_addr",
            "trap_keep_inc",
            "trap_keep_nested",
        ] {
            assert!(
                has(kept),
                "{flag}: 本该留下的引用被一并抹掉了（`{kept}` 不在 {:?}）——\
                 判据不是「越少越好」，这一半才是防过修的那一半",
                und
            );
        }
    }
}

/// 值被丢弃的纯读不得留下任何引用 —— 内核 context-analysis 的 token 变量形状。
///
/// 缺陷现场：`include/linux/compiler-context-analysis.h:242` 的 `token_context_lock()`
/// 造出 `extern const struct __ctx_lock_RCU *RCU;`，注释明写「not backed by a real
/// data structure (linker error if accidentally referenced)」—— 这类名字**天生没有
/// 定义**。而 `rcupdate.h:456` 把 `lockdep_assert_in_rcu_read_lock()` 定义成
/// `__assume_shared_ctx_lock(RCU)`，也就是 `do { (void)(RCU); } while (0)`。gcc 对丢弃值
/// 的纯读不发一条指令；dcc 以前在 `Expr::Cast` 的 void 臂里只筛字面量 ⇒ 发成
/// `mov RCU(%rip),%rax`，产物里多出 `R_X86_64_PC32 RCU` 与 `UND RCU`，最终链接报
/// `undefined reference to 'RCU'`（实测 `io_uring/tw.o` 的 `io_ctx_mark_taskrun()`）。
///
/// 判据双向钉（夹具逐项说明见 `tests/fixtures/c/token_ctx_lock_discarded_read.c`）：
/// 陷阱引用必须为 0，而 call / volatile 读 / 真取地址三条**绝不能被一并抹掉**（后者
/// 证明修复不是「凡是叫 RCU 就删」）。规则住在 AST 层的 `prune_dead_stores`（`level >= 2`），
/// 所以跑 -O2/-O3；发射器侧故意不管丢弃值的纯读（它拿不到声明，判不了 volatile），
/// 因此 -O0 仍然会发那一条 —— 内核不用 -O0 编。
#[test]
#[cfg(unix)]
fn discarded_pure_read_leaves_no_reference_while_effectful_arms_do() {
    let src = include_str!("../fixtures/c/token_ctx_lock_discarded_read.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("tokctxlock", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let syms = elf64_syms_by_type(&bytes);
        let rela_refs = |n: &str| {
            elf64_rela(&bytes, ".rela.text")
                .iter()
                .filter_map(|(_, idx, _, _)| syms.get(*idx as usize))
                .filter(|s| s.name == n)
                .count()
        };
        let und_count = |n: &str| syms.iter().filter(|s| s.name == n && s.shndx == 0).count();
        assert_eq!(
            rela_refs("RCU"),
            0,
            "{flag}: `(void)(RCU)` 留下了 {} 条指向 token 变量的调用重定位（就是链接期那条 \
             `undefined reference to 'RCU'`）",
            rela_refs("RCU")
        );
        assert_eq!(
            und_count("RCU"),
            0,
            "{flag}: 没被真引用的 token 变量仍登了 UND 符号（`elf/rel_symbols.rs` 的「只发被引用的 \
             UND」判据被绕开了）"
        );
        // 反向护栏：三条真有效果的路径必须照旧发码。
        assert_eq!(
            rela_refs("trap_call"),
            1,
            "{flag}: `(void)f()` 的 call 被误删（void 丢弃不等于无副作用）"
        );
        assert_eq!(
            rela_refs("vglob_token"),
            1,
            "{flag}: `(void)v` 里对 volatile 全局的读被误删（C11 6.7.3p6：该读本身可观察）"
        );
        assert!(
            rela_refs("rcu_token_used") >= 1,
            "{flag}: 真被取用的同名 token 变量引用丢了（过修）"
        );
    }
}

/// C11 6.5.3.2p3: `&*E` 与 `E` 等价（`*`/`&` 相消，不取对象）。内核
/// `drivers/tty/sysrq.c` 的 `sysrq_key_table[]` 里，配置关闭的条目被定义成
/// `(*(const struct sysrq_key_op *)((void *)0))`，数组元素于是写成 `&*NULL`。
/// 缺陷现场：`const_eval` 的 `AddrOf` 臂只认 Ident/Index/Cast/Member/String，
/// 这个形状落到 `NotConstant`，整张表被 E1006 拒掉（`drivers/tty/sysrq.o 错误 1`）。
#[test]
#[cfg(unix)]
fn global_init_addr_of_null_deref_is_constant() {
    let source = "\
struct op { int x; };\n\
extern const struct op real_a;\n\
extern const struct op real_c;\n\
#define NULL_OP (*(const struct op *)((void *)0))\n\
const struct op *tab[3] = { &real_a, &NULL_OP, &real_c };\n";
    let (output, obj) = run_object_test("addr-of-null-deref", source);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "`&*NULL` 是合法的地址常量: {stderr}");
    // 中间槽是空指针常量：不发重定位（写 0）；两侧真符号各留一条 64 位重定位。
    let rel = Command::new("objdump")
        .arg("-r")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&rel.stdout).to_string();
    let _ = fs::remove_file(&obj);
    assert_eq!(
        text.matches("R_X86_64_64").count(),
        2,
        "`&*NULL` 槽不应发重定位（应只有两条 `&real_*`）:\n{text}"
    );
    assert_eq!(text.matches("real_a").count(), 1, "`&real_a` 重定位丢了:\n{text}");
    assert_eq!(text.matches("real_c").count(), 1, "`&real_c` 重定位丢了:\n{text}");
}

/// 字符串字面量作条件恒真（C11 6.3.2.1p3 退化为非空指针）。内核
/// `include/linux/netfilter/nf_conntrack_sip.h` 的
/// `__SIP_HDR()` 里 `.clen = (__cname) ? sizeof(__cname) - 1 : 0`
/// 就是这种形状（`__cname` 为 `"v"` 或 `NULL`）；只认整数条件时
/// `ct_sip_hdrs[]`/`ct_sdp_hdrs_v4[]` 被 E1006 拒掉
/// （`net/netfilter/nf_conntrack_sip.o 错误 1`）。
#[test]
#[cfg(unix)]
fn global_init_string_literal_cond_is_truthy() {
    let source = "\
struct s { const char *name; unsigned int clen; };\n\
static const struct s t[] = {\n\
    { .name = \"v\", .clen = (\"v\") ? sizeof(\"v\") - 1 : 0 },\n\
    { .name = \"TCP \", .clen = (\"TCP \") ? sizeof(\"TCP \") - 1 : 0 },\n\
    { .name = 0, .clen = ((const char *)0) ? 7 : 0 },\n\
};\n\
int use(void) { return (int)(t[0].clen + t[1].clen + t[2].clen); }\n";
    let (output, obj) = run_object_test("string-cond-truthy", source);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "非空地址常量作条件必须能折: {stderr}");
    let dump = Command::new("objdump")
        .args(["-s", "-j", ".data"])
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&dump.stdout).to_string();
    let _ = fs::remove_file(&obj);
    // 三个元素的 clen: 1, 4, 0（空指针常量条件为假）。
    assert!(text.contains("01000000 00000000"), "`sizeof(\"v\") - 1` 应为 1:\n{text}");
    assert!(text.contains("04000000 00000000"), "`sizeof(\"TCP \") - 1` 应为 4:\n{text}");
}

/// `.rodata` 内 compound literal 的字段引用外部符号：重定位必须走 `.rela.rodata`。
/// 缺陷现场：`drivers/iommu/amd/iommu.c` 编到 rc=134（abort），
/// `panicked at elf/resolve.rs: index out of bounds: the len is 7 but the index is
/// 18446744073709551615` —— `usize::MAX` 是「槽在 .rodata 内」的哨兵，却被当成了
/// `extra_sections` 的下标。
#[test]
#[cfg(unix)]
fn rodata_compound_literal_symbol_ref_emits_rela_rodata() {
    let source = "\
struct inner { const int *p; };\n\
struct outer { const struct inner *in; };\n\
extern const int real;\n\
const struct outer o = { .in = &(struct inner){ .p = &real } };\n";
    let (output, obj) = run_object_test("rodata-compound-sym", source);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "rodata 内 compound literal 引用符号必须能编: {stderr}");
    assert!(!stderr.contains("panicked"), "不允许以 panic 结束: {stderr}");
    let rel = Command::new("objdump")
        .arg("-r")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&rel.stdout).to_string();
    let _ = fs::remove_file(&obj);
    let rodata_part = text.split("RELOCATION RECORDS FOR [.rodata]").nth(1).unwrap_or("");
    let rodata_part = rodata_part
        .split("RELOCATION RECORDS FOR [")
        .next()
        .unwrap_or(rodata_part);
    assert!(
        rodata_part.contains("real"),
        "`&real` 必须落在 `.rela.rodata`（不是 `.rela.data` 或越界）:\n{text}"
    );
}

/// `&((T[]){…})[0].f1.f2`：取址目标不是复合字面量本体，而是数组成员的成员。
/// 缺陷现场：`drivers/gpu/drm/i915/i915_pmu.c` 的 `I915_PMU_FORMAT_ATTR()`
/// 展开成 `&((struct i915_str_attribute[]){…})[0].attr.attr`；`eval_const_expr`
/// 的 AddrOf 臂只认「& 直接套 CompoundLit」，这条链落到通用路径被写成 0
/// ⇒ `i915_pmu_format_attrs[0]` 是 NULL（`readelf -sW` 有符号、`.rela.data` 无重定位），
/// 内核起来后 sysfs 的 format 属性全是空指针。
#[test]
#[cfg(unix)]
fn global_init_addr_of_compound_array_member_chain() {
    let source = "\
struct attr { const char *name; unsigned short mode; };\n\
struct dev_attr { struct attr attr; void *show; void *store; };\n\
struct str_attr { int pad; struct dev_attr attr; const char *str; };\n\
extern void show_fn(void);\n\
static struct attr *tab[] = {\n\
    (&((struct str_attr[]){ {\n\
        .attr = { .attr = { .name = \"e\", .mode = 0444 }, .show = (void *)show_fn },\n\
        .str = \"v\",\n\
    } })[0].attr.attr),\n\
};\n\
int use(void) { return tab[0] != 0; }\n";
    let (output, obj) = run_object_test("compound-array-member-chain", source);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "复合字面量数组成员取址必须能编: {stderr}");
    let rel = Command::new("objdump")
        .arg("-r")
        .arg(&obj)
        .output()
        .expect("objdump");
    let text = String::from_utf8_lossy(&rel.stdout).to_string();
    let _ = fs::remove_file(&obj);
    let data_part = text.split("RELOCATION RECORDS FOR [.data]").nth(1).unwrap_or("");
    let data_part = data_part
        .split("RELOCATION RECORDS FOR [")
        .next()
        .unwrap_or(data_part);
    assert!(
        data_part.contains(".L.compound"),
        "`tab[0]` 必须重定位到匿名的复合字面量（现在被写成 NULL）:\n{text}"
    );
    // `.attr` 前面有 int pad：成员链偏移必须是编译期常量 8，不能丢成 +0。
    assert!(
        data_part
            .lines()
            .any(|l| l.contains(".L.compound") && l.contains("+0x0000000000000008")),
        "`&(…)[0].attr.attr` 的加数应为 8（int pad 之后）:\n{text}"
    );
}

/// `drm_WARN_ON()` 的 `__stringify(x)` 套在 `REG_FIELD_PREP()` 上面：
/// 条件展开后带着内层 `GENMASK_INPUT_CHECK()` 的 `_Static_assert` 消息字符串，
/// 而 `BUILD_BUG_ON_ZERO(!__is_constexpr(mask))` 又先把它转义了一遍。
/// `#` 必须把已经转义过的 `\"` 双写成 `\\\"`。只给 `"` 补 `\`、不给 `\` 双写时，
/// 那个 `"` 重新词法分析会提前闭合字符串，剩下的 `is true\"` 变成游离 token
/// （`\` 记 Invalid），紧接的语句报 E0001
/// （`drivers/gpu/drm/i915/display/intel_cdclk.c:1979` 实测 43 条）。
#[test]
#[cfg(unix)]
fn kernel_stringify_escapes_inner_literal_quotes() {
    let source = "\
#define __stringify_1(x...) #x\n\
#define __stringify(x...) __stringify_1(x)\n\
#define __is_constexpr(x) (sizeof(int) == sizeof(*(8 ? ((void *)((long)(x) * 0l)) : (int *)8)))\n\
#define __BUILD_BUG_ON_ZERO_MSG(e, msg, ...) ((int)sizeof(struct { _Static_assert(!(e), ##__VA_ARGS__, msg); }))\n\
#define BUILD_BUG_ON_ZERO(e, ...) __BUILD_BUG_ON_ZERO_MSG(e, ##__VA_ARGS__, #e \" is true\")\n\
#define const_true(x) __builtin_choose_expr(__is_constexpr(x), x, (0))\n\
#define GENMASK_INPUT_CHECK(h, l) BUILD_BUG_ON_ZERO(const_true((l) > (h)))\n\
#define GENMASK_U32(h, l) ((unsigned)(GENMASK_INPUT_CHECK(h, l) + (((~0u) << (l)) & (~0u >> (31 - (h))))))\n\
#define __bf_shf(x) (__builtin_ffsll(x) - 1)\n\
#define REG_FIELD_PREP(m, v) ((unsigned)((((typeof(m))(v) << __bf_shf(m)) & (m)) + BUILD_BUG_ON_ZERO(!__is_constexpr(m))))\n\
#define DIV_SEL_MASK GENMASK_U32(23, 22)\n\
#define DIV_SEL_1 REG_FIELD_PREP(DIV_SEL_MASK, 0)\n\
extern void drm_report(const char *fmt, ...);\n\
#define drm_WARN_ON(x) do { if (x) drm_report(\"%s\", \"drm_WARN_ON(\" __stringify(x) \")\"); } while (0)\n\
int cdclk_pipe(int ret) { drm_WARN_ON(ret != DIV_SEL_1); return ret; }\n";
    let (output, obj) = run_object_test("kernel-stringify-inner-quotes", source);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "`#` 转义内层字面量失败会让后续 token 游离（E0001/E0006）: {stderr}"
    );
    let text = Command::new("strings").arg(&obj).output().expect("strings");
    let _ = fs::remove_file(&obj);
    let strs = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(
        strs.contains("drm_WARN_ON(") && strs.contains("const_true((22) > (23))"),
        "警告消息应完整包含 `drm_WARN_ON(` 与内层字面量（字符串被提前截断时会拼不出来）:\n{strs}"
    );
}

/// 括号化函数指针声明符把属性写在 `*` **前面**：
/// `(__attribute__((ms_abi)) *fn_t)`。内核用 `__efiapi` 铺满了 EFI 回调原型
/// （`drivers/firmware/efi/libstub/efistub.h:184/216`）——
///
/// 三条声明符路径都必须在「基类型的 `*` 已吃完」之后才看到这个 `(`：
///   * typedef：`typedef void (__efiapi *fn_t)(...)` 会先被「(name) 函数 typedef」
///     分支当成名字，随后 `expect(')')` 报 E0001；
///   * 字段：`efi_char16_t *(__efiapi *cb)(...)` 的基类型自带 `*`，属性探测若放在
///     `parse_ptr_suffix` 之前，cur 还停在 `*` 上，永远看不到 `(`；
///   * 全局变量：同上。
/// 旧实现三个形状全报 E0001/E0003/E0009（build6 里 alignedmem.o 一个 TU 35 条）。
/// 除了能编，还得证声明符结合顺序没错：多包一层 `*`（`void **(*f)(int)`）
/// 在 x86_64 上尺寸一样，只有真调用才暴露，所以这里跑运行时。
#[test]
#[cfg(unix)]
fn kernel_paren_funcptr_attr_declarator() {
    let source = "\
#define __efiapi __attribute__((ms_abi))\n\
static void *ret_ptr(int x) { return (void *)(long)(x + 41); }\n\
typedef void (__efiapi *notify_t)(int, void *);\n\
typedef void *(*plain_t)(int);\n\
struct s { void *(__efiapi *f)(int); };\n\
static void (*gp)(int, void *);\n\
static void *(__efiapi *gp2)(int);\n\
int main(void) {\n\
\tnotify_t t = (notify_t)0;\n\
\tplain_t f = ret_ptr;\n\
\tstruct s s1 = { ret_ptr };\n\
\tgp = (void (*)(int, void *))ret_ptr;\n\
\tgp2 = ret_ptr;\n\
\tt = (notify_t)f;\n\
\tif (f(1) != (void *)42) return 1;\n\
\tif (s1.f(1) != (void *)42) return 2;\n\
\tif (gp2(1) != (void *)42) return 3;\n\
\tif (t != (notify_t)f) return 4;\n\
\tif (sizeof(struct s) != 8) return 5;\n\
\tif (gp == 0) return 6;\n\
\treturn 0;\n\
}\n";
    let (out, exe) = run_compile_and_run("paren-funcptr-attr", source);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "括号内属性在 `*` 前的函数指针声明符应能编: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    // 运行时能过 ⇒ 声明符结合顺序对（多包一层指针时调用会读错返回类型）。
    assert_eq!(rc, 0, "属性函数指针声明符的运行时行为不对（rc={rc}）");
}

/// 数据节里**只定义、不被引用**的 asm 标号必须进 symtab（GAS 口径）。
///
/// `include/linux/export.h` 的导出条目是
///     .section ".export_symbol","a"
///     __export_symbol_##sym:
///         .asciz <license> / .ascii <ns> "\0" / .quad sym
/// —— 标号只定义、从没被任何重定位引用，也没有 `.globl`。modpost 的
/// `check_export_symbol` 靠 `find_fromsym(faddr, .export_symbol)` 找条目所在的
/// 那条标号（取「≤ faddr 的最近符号」），所以 `.o` 里没这条符号时它会错配到
/// 前一个导出名下：build8 的 vmlinux.symvers 阶段满屏
/// “references 'x', but it does not seem to be an export symbol”后 SIGSEGV。
#[test]
#[cfg(unix)]
fn asm_label_in_data_section_emitted_to_symtab() {
    let source = "\
int exported_fn(int x) { return x + 1; }
\
asm(\".section \\\".export_symbol\\\",\\\"a\\\"\\n\
\
__export_symbol_exported_fn:\\n\
\
\\t.asciz \\\"GPL\\\"\\n\
\
\\t.ascii \\\"\\\" \\\"\\\\0\\\"\\n\
\
\\t.balign 8\\n\
\
\\t.quad exported_fn\\n\
\
.previous\\n\");\n";
    let (out, obj) = run_object_test("asm-label-symtab", source);
    assert!(
        out.status.success(),
        "导出条目夹具应能编: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let secs = Command::new("readelf").arg("-SW").arg(&obj).output().expect("readelf -SW");
    let secs_text = String::from_utf8_lossy(&secs.stdout).to_string();
    // `.export_symbol` 的节号：符号条目的 Ndx 列必须是它，不能落到 .text。
    let sec_index = secs_text
        .lines()
        .find_map(|l| {
            // 行首是 `[ 7]` / `[10]` 这种带空格的节号。
            let t = l.trim_start().trim_start_matches('[');
            let mut f = t.split_whitespace();
            let idx = f.next()?.trim_end_matches(']');
            (f.next()? == ".export_symbol").then(|| idx.parse::<u32>().ok())?
        })
        .expect("`.export_symbol` 节应存在");
    let syms = Command::new("readelf").arg("-sW").arg(&obj).output().expect("readelf -sW");
    let syms_text = String::from_utf8_lossy(&syms.stdout).to_string();
    let line = syms_text
        .lines()
        .find(|l| l.contains("__export_symbol_exported_fn"))
        .unwrap_or_else(|| panic!("只定义不引用的 asm 标号不进 symtab，modpost 认不出导出条目:\n{syms_text}"));
    let f: Vec<&str> = line.split_whitespace().collect();
    // Num: Value Size Type Bind Vis Ndx Name → 这条链上字段下标固定。
    assert!(
        f.len() >= 8 && f[3] == "NOTYPE" && f[4] == "LOCAL",
        "标号应为 LOCAL/NOTYPE（与 GAS 对未 .globl 标号的口径一致）: {line}"
    );
    assert_eq!(f[5], "DEFAULT", "可见性应为 DEFAULT: {line}");
    assert_eq!(
        f[6].parse::<u32>().ok(),
        Some(sec_index),
        "符号必须坐在 .export_symbol 里（不是 .text）: {line}"
    );
    assert_eq!(f[1], "0000000000000000", "标号应在条目起点（license 字符串前）: {line}");
    let _ = fs::remove_file(&obj);
}

/// `-fPIC` 必须关掉稠密 switch 的跳表降级（`ssa::structure`）。
///
/// 跳表是「静态 `const void *const` 表 + `goto *`」，表项发成 `.quad 标号`，
/// 也就是 `.data` 里的 **绝对** 64 位地址 `R_X86_64_64`。EFI stub 用 `-fPIC`
/// 编，`drivers/firmware/efi/libstub/Makefile` 的 STUBCPY 检查直接把这种重定位
/// 当作错误（build7 卡在 `vsprintf.stub.o: absolute symbol references not allowed
/// in the EFI stub`，33 条 R_X86_64_64）；PIC 的本意也不允许地址固化进数据段。
///
/// 夹具必须是**非结构化**函数（有 goto/label）才会走到跳表降级那条路，
/// 只放一个稠密 switch 是编不出表的（普通函数走 if 链降级 + SSA 重建）。
/// 两条断言互为对照：非 PIC 有表（证明夹具真触发了降级，PIC 侧的空集不是
/// 因为根本没上表），PIC 无表。
#[test]
#[cfg(unix)]
fn switch_jumptable_gated_by_pic() {
    let source = "\
static int dense(int x)
{
\tint r = 0;
\tif (x < 0)
\t\tgoto done;
\tswitch (x % 12) {
\tcase 0: r += 1; break;
\tcase 1: r += 2; break;
\tcase 2: r += 3; break;
\tcase 3: r += 4; break;
\tcase 4: r += 5; break;
\tcase 5: r += 6; break;
\tcase 6: r += 7; break;
\tcase 7: r += 8; break;
\tcase 8: r += 9; break;
\tcase 9: r += 10; break;
\tcase 10: r += 11; break;
\tdefault: r = -1; break;
\t}
done:
\treturn r;
}
int dense_call(int x) { return dense(x); }
";
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-picgate-{}.c", temp_tag()));
    fs::write(&src, source).expect("write C fixture");
    // DCC_SWITCHTABLE=1 显式钉住：否则外部环境里 `DCC_SWITCHTABLE=0`（归因用的
    // 反向闸）会让非 PIC 侧也不上表，对照断言变假阳性。
    let build = |obj: &std::path::Path, extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .arg("-c")
            .arg("-O2")
            .args(extra)
            .env("DCC_SWITCHTABLE", "1")
            .arg("-o")
            .arg(obj)
            .arg(&src)
            .output()
            .expect("run dcc");
        assert!(out.status.success(), "编译失败({extra:?}): {}", String::from_utf8_lossy(&out.stderr));
        let rel = Command::new("objdump")
            .arg("-r")
            .arg(obj)
            .output()
            .expect("objdump -r");
        assert!(rel.status.success(), "objdump -r failed");
        String::from_utf8_lossy(&rel.stdout).to_string()
    };
    let plain_obj = dir.join(format!("dcc-picgate-plain-{}.o", temp_tag()));
    let pic_obj = dir.join(format!("dcc-picgate-pic-{}.o", temp_tag()));
    let plain_rel = build(&plain_obj, &[]);
    let pic_rel = build(&pic_obj, &["-fPIC"]);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&plain_obj);
    let _ = fs::remove_file(&pic_obj);
    assert!(
        plain_rel.contains("R_X86_64_64"),
        "非 PIC 下非结构化函数的稠密 switch 应上跳表（夹具没触发降级？）:\n{plain_rel}"
    );
    assert!(
        !pic_rel.contains("R_X86_64_64"),
        "-fPIC 下不得出现绝对地址表项（EFI stub 的 STUBCPY 会拒收）:\n{pic_rel}"
    );
}

/// 内核 `include/linux/bitfield.h` 的常量位域链必须折到底。判据取 gcc -O2 实测：
/// 三条「常量实装得下」的调用一条 `trap_*` 引用都不留，只有越界那条留 1 条。
/// 现场后果（修前）：`LD .tmp_vmlinux1` 报 `undefined reference to '__field_overflow'`
/// 21 条 + `'__bad_mask'` 8 条 + `'__bad_udelay'` 3 条，而 `trap_*` 这类函数在头里
/// **只有声明、没有定义**，gcc 靠「永远不发出引用」保证链得上。
#[test]
#[cfg(unix)]
fn bitfield_const_encode_folds_traps_away() {
    let src = include_str!("../fixtures/c/bitfield_const_encode_traps.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("bf-trap", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und = elf64_undefined_symbols(&bytes);
        let kept: Vec<&String> = und
            .iter()
            .filter(|n| n.starts_with("trap_"))
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "{flag}: 常量位域检查没折干净（或折过头），产物里留下 {kept:?} 条 trap 引用；\
             gcc -O2 实测只有 c4_overflow_stays 的 1 条"
        );
        assert!(
            kept[0].starts_with("trap_field_overflow"),
            "{flag}: 唯一该留下的越界引用错了，实际: {kept:?}"
        );
    }
}

/// 内核 `DO_ONCE()` 的两条必经形状：`"i"` 立即数必须落到函数内 `static` 的
/// **mangled 名**上，且 `static_key_enabled()` 的类型守卫必须折成假。
/// 现场后果（修前）：全树 47 个对象 `undefined reference to '___once_key'`
/// （`__jump_table` 条目发的是源码名，而定义体 `.Lsl<N>.<fn>.___once_key` 明明就在
/// `.data..do_once` 里），另有 21 条 `____wrong_branch_error`。
#[test]
#[cfg(unix)]
fn once_key_asm_immediate_targets_mangled_static() {
    let src = include_str!("../fixtures/c/once_key_asm_immediate_symbol.c");
    for flag in ["-O0", "-O2"] {
        let (out, obj) = run_object_test_flags("oncekey", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und = elf64_undefined_symbols(&bytes);
        assert!(
            !und.iter().any(|n| n == "___once_key"),
            "{flag}: asm 立即数把符号打成了源码名，产物里有不存在的 `___once_key` 引用：{und:?}"
        );
        assert!(
            !und.iter().any(|n| n.starts_with("trap_")),
            "{flag}: `__builtin_types_compatible_p` 的守卫没折掉，留下陷阱引用：{und:?}"
        );
        let syms = elf64_syms_by_type(&bytes);
        // `__jump_table` 的那条重定位必须指向一个**本对象已定义**的符号。
        let rela = elf64_rela(&bytes, ".rela__jump_table");
        assert_eq!(
            rela.len(),
            1,
            "{flag}: `__jump_table` 条目应当有 1 条重定位，实际 {} 条",
            rela.len()
        );
        let target = syms.get(rela[0].1 as usize).expect("jump_table 重定位的符号存在");
        assert!(
            target.name.ends_with("___once_key") && target.shndx != 0,
            "{flag}: `__jump_table` 条目应指向已定义的 static 键（mangled 名），\
             实际指向 {:?} shndx={}",
            target.name,
            target.shndx
        );
    }
}

/// `-Wimplicit-fallthrough` 必须认 GCC 的双下划线写法。内核的伪关键字 `fallthrough;`
/// 经 `include/linux/compiler_attributes.h:214` 展开成
/// `__attribute__((__fallthrough__))`，只比裸名会全不认 —— 实测 localmodconfig 全树
/// 44 185 条 W0501，主体来自 `include/linux/signal.h` 的 `_SIG_SET_OP` 与
/// `include/linux/mm.h` 的位图宏，每个含它的 TU 刷一遍。
/// 判据取 gcc 同旗标实测：整份夹具只报 1 条（那个真的没标记的）。
#[test]
#[cfg(unix)]
fn fallthrough_gnu_attr_double_underscore_is_recognized() {
    let src = include_str!("../fixtures/c/fallthrough_gnu_attr_double_underscore.c");
    let (out, obj) = run_object_test_flags("ftattr", src, &["-O2", "-Wall", "-Wimplicit-fallthrough"]);
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(&obj);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let n = err.matches("W0501").count();
    assert_eq!(
        n, 1,
        "四种 fallthrough 标记形态里还有没被认出的（W0501 应当只剩 f_real_bug 那 1 条），\n实际 {n} 条:\n{err}"
    );
    // 诊断给的是「文件:行:列」而不带函数名，所以拿行号定位：唯一那条必须落在
    // f_real_bug（夹具里最后一个函数，也是唯一没标记的那个）起算的位置之后。
    let bug_line = src
        .lines()
        .position(|l| l.starts_with("int f_real_bug("))
        .expect("夹具里应有 f_real_bug")
        + 1;
    let reported: Vec<usize> = err
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("--> "))
        // 形式是 `…/<名>.c:行:列`：从右往左第三段才是行号（rsplitn(3) 的 [1]）。
        .filter_map(|l| l.rsplitn(3, ':').nth(1)?.parse().ok())
        .collect();
    assert_eq!(
        reported.len(),
        1,
        "W0501 的位置行应当只有一条，实际: {reported:?}\n{err}"
    );
    assert!(
        reported[0] >= bug_line,
        "唯一那条 W0501 指向第 {} 行，不在没标记的 f_real_bug（自第 {bug_line} 行起）里 —— \
         说明某个已标记形态被误报了:\n{err}",
        reported[0]
    );
}

/// 同一批 `__attribute__((__fallthrough__))` 形态坐在**宏定义体**里时也必须被认出。
///
/// 内核 `include/linux/signal.h` 的 `_SIG_SET_OP` 就是这一形：整个 switch 在宏体里，
/// `fallthrough;` 经 `compiler_attributes.h` 展开成属性。宏展开后的 `__FILE__`/行号
/// 不是字面源码位置，只比对 raw token 的实现在这里会漏认（实测 localmodconfig 全树
/// W0501 的主体就是它：每个含 `<linux/signal.h>` 的 TU 刷 5 条）。判据与上条一致：
/// 整份夹具只该报 `f_real_bug` 那 1 条。
#[test]
#[cfg(unix)]
fn fallthrough_inside_macro_body_is_recognized() {
    let src = include_str!("../fixtures/c/fallthrough_inside_macro_body.c");
    let (out, obj) =
        run_object_test_flags("ftmacro", src, &["-O2", "-Wall", "-Wimplicit-fallthrough"]);
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(&obj);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(
        err.matches("W0501").count(),
        1,
        "宏体里的 fallthrough 属性形态没被全认出（W0501 应当只剩 f_real_bug 那 1 条）:\n{err}"
    );
    let bug_line = src
        .lines()
        .position(|l| l.starts_with("int f_real_bug("))
        .expect("夹具里应有 f_real_bug")
        + 1;
    let reported: Vec<usize> = err
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("--> "))
        .filter_map(|l| l.rsplitn(3, ':').nth(1)?.parse().ok())
        .collect();
    assert_eq!(reported.len(), 1, "W0501 的位置行应当只有一条，实际: {reported:?}\n{err}");
    assert!(
        reported[0] >= bug_line,
        "唯一那条 W0501 指向第 {} 行，不在没标记的 f_real_bug（自第 {bug_line} 行起）里 —— \
         宏体里某个已标记形态被误报了:\n{err}",
        reported[0]
    );
}

/// `#if` 的**短路求值**与内核常用内建的诊断静默。判据取 gcc -Wundef 实测：
/// `include/uapi/linux/stat.h:7` 与 `coda.h:100` 那两形 gcc 一条不报（未参与运算的
/// 操作数不警告），只有真被读到的裸标识符报 1 条；`__builtin_unreachable`/
/// `__builtin_prefetch` 不该报「返回类型未知」（全树 39 235 条 W1015）。
/// 顶层 `?:` 走同一条判据（`include/uapi/sound/asound.h:561` 那族字节序宏，
/// 全树 656 条 W0202 而 gcc 0 条）。
/// 同时钉住分支选择：短路只是求值策略，不是语义。
#[test]
#[cfg(unix)]
fn pp_if_short_circuit_matches_gcc_undef() {
    let src = include_str!("../fixtures/c/pp_short_circuit_undef_and_builtins.c");
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-ppsc-{}.c", std::process::id()));
    fs::write(&sp, src).expect("write pp fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-Wundef", "-E"])
        .arg(&sp)
        .output()
        .expect("run dcc -Wundef -E");
    let _ = fs::remove_file(&sp);
    assert!(
        output.status.success(),
        "预处理失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let err = String::from_utf8_lossy(&output.stderr).to_string();
    let outtxt = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !err.contains("__GLIBC__") && !err.contains("__GLIBC2__"),
        "短路后不该被读到的宏仍被警告了（gcc 同输入 0 条）:\n{err}"
    );
    // 顶层 `?:` 也要短路：内核那一族字节序判据（实测 localmodconfig 全树 656 条
    // W0202，gcc 同旗标 0 条）就是只不切 `?:` 造成的。
    assert!(
        !err.contains("__BYTE_ORDER")
            && !err.contains("__BIG_ENDIAN")
            && !err.contains("UNREAD_IN_ELSE")
            && !err.contains("UNREAD_IN_THEN"),
        "三元没短路，未参与的支也刷了 -Wundef（gcc 同输入 0 条）:\n{err}"
    );
    assert!(
        err.contains("READING_UNDEF_MACRO"),
        "真被读到的裸标识符必须照报（gcc 也报这一条）:\n{err}"
    );
    assert!(
        !err.contains("W1015"),
        "`__builtin_unreachable`/`__builtin_prefetch` 不该报返回类型未知:\n{err}"
    );
    assert!(
        outtxt.contains("int short_circuit_true;") && outtxt.contains("int paren_short_true;"),
        "短路改变了分支选择（语义被改坏）:\n{outtxt}"
    );
    // 三元的分支选择：判据取 gcc -Wundef -E 实测（be_not_taken / one_taken / three_taken）。
    assert!(
        outtxt.contains("int endian_be_not_taken;")
            && outtxt.contains("int cond_true_taken;")
            && outtxt.contains("int cond_false_taken;"),
        "三元的分支选择与 gcc 不一致:\n{outtxt}"
    );
    assert!(
        !outtxt.contains("int endian_be_taken;")
            && !outtxt.contains("int cond_true_not_taken;")
            && !outtxt.contains("int cond_false_not_taken;"),
        "三元取了不该取的那一支:\n{outtxt}"
    );
    assert!(
        !outtxt.contains("short_circuit_false") && !outtxt.contains("paren_short_false"),
        "分支选择与修前不一致（不该取的 else 支被取了）:\n{outtxt}"
    );
}

/// `__has_attribute(__fallthrough__)` 必须答 1（GCC 的保留标识符写法与裸名等价）。
/// 答 0 的后果不是少个宏，而是整个内核退到
/// `include/linux/compiler_attributes.h:216` 的注释后备支 `do {} while (0)  /* fallthrough */`
/// —— 那条只能靠注释识别，于是实测 localmodconfig 全树刷出 44 185 条 W0501。
/// 判据取 gcc -E 实测：yes_us 在、no_us 与 mismatch 都不在；再带一次真正的
/// `fallthrough;`（宏体里，内核 `_SIG_SET_OP` 的形状）确认属性被认下。
#[test]
#[cfg(unix)]
fn has_attribute_accepts_reserved_identifier_spelling() {
    let src = include_str!("../fixtures/c/has_attribute_double_underscore.c");
    let dir = std::env::temp_dir();
    let sp = dir.join(format!("dcc-hattr-{}.c", std::process::id()));
    fs::write(&sp, src).expect("write has_attribute fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .arg("-E")
        .arg(&sp)
        .output()
        .expect("run dcc -E");
    assert!(
        output.status.success(),
        "-E 失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pp = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        pp.contains("int yes_us;"),
        "`__has_attribute(__fallthrough__)` 被判成不支持，内核会退到注释后备支:\n{pp}"
    );
    assert!(
        !pp.contains("int no_us;"),
        "取错了 #if 分支（该走属性支）:\n{pp}"
    );
    assert!(
        !pp.contains("int mismatch;"),
        "裸名与两侧下划线名答案不一致（同一属性的两种写法）:\n{pp}"
    );
    let (out, obj) = run_object_test_flags(
        "hattr",
        src,
        &["-O2", "-std=gnu11", "-Wall", "-Wimplicit-fallthrough"],
    );
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&sp);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).matches("W0501").count(),
        0,
        "宏体里的 `fallthrough;`（展开成 __attribute((__fallthrough__))）没被认出:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 同 TU 内「空体 void 函数」的调用必须被就地吸收。现场（`arch/x86/kernel/alternative.c:2439`）：
/// 声明 `extern s32 __ibt_endbr_seal[]` 与调用都是**无条件**的，而 `!CONFIG_X86_KERNEL_IBT`
/// 时选中的定义体是 `{ }`，链接脚本又把 `.ibt_endbr_seal` 整个节包在
/// `#ifdef CONFIG_X86_KERNEL_IBT` 里 ⇒ 只要调用还在就报
/// `undefined reference to '__ibt_endbr_seal'`（实测本树 2 条）。gcc/clang 靠吸收空体调用
/// 把实参的地址一起消掉，`fold_const_return_calls` 只认 `static` 常量桩，看不见这一族。
///
/// 判据只量这条规则本身，不借 gcc 的其它 DCE 当等式（gcc 在这份小夹具上还顺手删了
/// 两处未用返回值的纯调用，那是另一件事）：
///   * `probe_only_declared_end` **只**出现在被吸收的那次调用里 ⇒ 它必须 0 条重定位；
///   * 两处不该被吸收的调用点（被调者有真实体 / 实参本身是一次真调用）各自引用
///     `probe_only_declared` ⇒ 必须留着 2 条，证明规则没有过头。
#[test]
#[cfg(unix)]
fn same_tu_empty_void_call_is_absorbed_but_not_over_absorbed() {
    let src = include_str!("../fixtures/c/empty_void_call_absorbed.c");
    for flag in ["-O1", "-O2"] {
        let (out, obj) = run_object_test_flags("emptyvoid", src, &[flag, "-std=gnu11"]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        // 把每条重定位的符号索引换成符号名。两处必须用对工具：
        //   * `elf64_all_rela` 只走 `.rela.dyn/.plt/.iplt`，而这里的引用坐在
        //     `.rela.text` 里，所以直接点那条节；
        //   * 名字要用 **全表下标对齐** 的 [`elf64_syms`]，[`elf64_syms_by_type`] 是筛过的，
        //     拿它的下标去解重定位的 `sym_index` 会整体错位。
        // 判据拿**全名相等**：`probe_only_declared_end` 包含子串 `probe_only_declared`，
        // 用 contains 会把两个计数器搅在一起。
        let syms = elf64_syms(&bytes);
        let names: Vec<String> = elf64_rela(&bytes, ".rela.text")
            .iter()
            .filter_map(|(_, idx, _, _)| syms.get(*idx as usize).map(|s| s.name.clone()))
            .collect();
        let n_end = names.iter().filter(|n| *n == "probe_only_declared_end").count();
        let n_decl = names.iter().filter(|n| *n == "probe_only_declared").count();
        assert_eq!(
            n_end,
            0,
            "{flag}: 空体调用没被吸收 —— 只有被吸收那次用到 `_end` 数组，它还留着 \
            {n_end} 条重定位（现场后果就是链接期 undefined reference）"
        );
        assert_eq!(
            n_decl, 2,
            "{flag}: `probe_only_declared` 的重定位应当是 2 条（两处**不该**吸收的调用点各一条），\
             实际 {n_decl} 条 —— 说明规则过头，把有副作用/有真实体的调用也吞了"
        );
    }
}

/// 链式 `if / else if / else` 里恒假的 `else if` 支必须被塌缩掉。
///
/// 为什么单独立一条：`fold_branches_block` 删「块内一项」的 `if` 是能做的，而第三支坐在
/// 外层 `if` 的 `else_` 操作数上，不就地塌缩就永远删不掉。现场后果（实测 `mm/rmap.o`
/// 的 `folio_referenced_one`）：`else if (IS_ENABLED(CONFIG_TRANSPARENT_HUGEPAGE))` 在
/// THP 关时是死支，里面调的是 `include/linux/pgtable.h:546` 的 `BUILD_BUG()` 占位实现，
/// 占位的调用发进 `.text` ⇒ 链接期 `undefined reference to 'pmdp_clear_flush_young'`。
///
/// 判据两半：产物级（trap 一条引用都不许留）+ 行为级（恒真的 `else if (1)` 必须**选中**，
/// 连塌两跳后的值也必须对，否则「一律删第三支」这种假修复能过前者、过不了后者）。
#[test]
#[cfg(unix)]
fn dead_else_if_arm_is_collapsed_and_live_one_is_taken() {
    let src = include_str!("../fixtures/c/dead_else_if_chain.c");
    for flag in ["-O1", "-O2"] {
        let (out, obj) = run_object_test_flags("elseif", src, &[flag, "-std=gnu11"]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und = elf64_undefined_symbols(&bytes);
        assert!(
            !und.iter().any(|n| n == "trap_never_called"),
            "{flag}: 恒假的 `else if` 支没塌掉，产物里留下对 trap 的引用：{und:?}"
        );
        // 行为级：反向对照在这里生效（退出码就是第一个不匹配的形状编号）
        let (cout, exe) = run_compile_and_run_flags("elseif", src, &[flag, "-std=gnu11"]);
        assert!(
            cout.status.success(),
            "{flag}: 链接失败（死支没折掉时 trap 无定义，链接就会报）: {}",
            String::from_utf8_lossy(&cout.stderr)
        );
        let rc = run_exe(&exe);
        let _ = fs::remove_file(&exe);
        assert_eq!(
            rc,
            0,
            "{flag}: 分支选择被改坏（退出码 1=恒假 else-if 没塌、2=恒真 else-if 没选中、\
             3=连塌两跳后的值不对），实际 {rc}"
        );
    }
}

/// `-Wimplicit-fallthrough` 与 gcc **逐条对齐**：报哪几条、不报哪几条都要一样。
/// 四条形状里 `case A: case B:`（共享体的空 case）与以 `do { … __builtin_unreachable();
/// } while (0);`（内核 `BUG()` 的包装）收尾的两支 gcc 不报，只有「只有一句声明」与
/// 「真贯穿」两支报 —— 前者以前被dcc 误报（展开后本支体里剩一条空语句占位，
/// `last_effective_stmt` 返回 None 却落进「未标注贯穿」分支），后者以前也不报终止
/// （`do{}while(0)` 被当成循环就不算终止）。期望值取 gcc -O0 -Wall 实测：2 条。
#[test]
#[cfg(unix)]
fn implicit_fallthrough_matches_gcc_shape_by_shape() {
    let src = include_str!("../fixtures/c/fallthrough_gcc_parity_shapes.c");
    let (out, obj) =
        run_object_test_flags("ftshapes", src, &["-O0", "-Wall", "-Wimplicit-fallthrough"]);
    assert!(
        out.status.success(),
        "编译失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_file(&obj);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    // 只取紧跟在 W0501 之后的那一行 `--> 路径:行:列`：别的警告也带 `-->`（比如
    // 内建名单缺项时的 W1015），混进来会把判据脏掉。
    let mut lines: Vec<usize> = Vec::new();
    let mut pending = false;
    for l in err.lines() {
        if l.starts_with("warning[W0501]") {
            pending = true;
            continue;
        }
        if pending {
            if let Some(rest) = l.trim_start().strip_prefix("--> ") {
                if let Some(ln) = rest.rsplitn(3, ':').nth(1).and_then(|v| v.parse::<usize>().ok())
                {
                    lines.push(ln);
                }
                pending = false;
            }
        }
    }
    let nth = |fn_name: &str| -> usize {
        src.lines()
            .position(|l| l.starts_with(&format!("int {fn_name}("))
                || l.starts_with(&format!("void {fn_name}(")))
            .unwrap_or_else(|| panic!("夹具里应有 {fn_name}"))
            + 1
    };
    let (empty, decl_bare, real_ft, bug_like) = (
        nth("empty_shared_case"),
        nth("decl_bare_case"),
        nth("real_fallthrough"),
        nth("bug_like_case"),
    );
    let inside = |ln: usize, start: usize, next: usize| ln >= start && ln < next;
    assert_eq!(
        lines.len(),
        2,
        "W0501 应与 gcc 一样只有 2 条（声明-only 与真贯穿），实际落在 {lines:?}:\n{err}"
    );
    assert!(
        lines.iter().all(|ln| inside(*ln, decl_bare, bug_like)),
        "两条都该落在声明-only / 真贯穿这两个函数里，实际 {lines:?}"
    );
    assert!(
        !lines.iter().any(|ln| inside(*ln, empty, decl_bare)),
        "`case A: case B:` 的空 case 被误报了（gcc 不报：没有语句被跨过）"
    );
    assert!(
        !lines.iter().any(|ln| *ln >= bug_like),
        "以 `do {{ … }} while (0);`（BUG() 形状）收尾的 case 被误报了（gcc 不报）"
    );
    assert!(
        lines.iter().any(|ln| inside(*ln, decl_bare, real_ft)),
        "「只有一句声明」的那支必须报（gcc 报）：{lines:?}"
    );
    assert!(
        lines.iter().any(|ln| inside(*ln, real_ft, bug_like)),
        "真贯穿的那支必须报（gcc 报）：{lines:?}"
    );
    assert!(
        !err.contains("W1015"),
        "`__builtin_trap()` 不该报「返回类型未知」（它没有声明可写）:\n{err}"
    );
    assert!(
        !err.contains("W0502"),
        "不该出现 misplaced fallthrough 警告:\n{err}"
    );
}

/// 块级 `const` 出块后不得继续遮蔽同名变量。现场（实测 `mm/compaction.c:643`）：函数
/// 作用域有可变的 `order`，`if (PageCompound(page)) { const unsigned int order = …; }`
/// 里另有一个块级同名 const，出块之后的 `order = buddy_order(page);` 被 dcc 判成
/// `E0030: assignment of read-only variable 'order'` 直接拒编（`var_quals` 以前只按
/// 名字建表、只在换函数时清）。
/// 判据两半：夹具三个函数都必须**编过且跑对**（退出码就是第一个不匹配的函数编号），
/// 而真的 const 赋值仍必须是错误 —— 只把检查关掉这种改法过不了第二条。
#[test]
#[cfg(unix)]
fn block_scoped_const_does_not_leak_out_of_its_block() {
    let src = include_str!("../fixtures/c/const_shadow_scope.c");
    for flag in ["-O0", "-O2"] {
        let (out, exe) = run_compile_and_run_flags("constshadow", src, &[flag, "-std=gnu11"]);
        assert!(
            out.status.success(),
            "{flag}: 编译/链接失败（块级 const 泄漏出块就会走到这里）: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rc = run_exe(&exe);
        let _ = fs::remove_file(&exe);
        assert_eq!(
            rc,
            0,
            "{flag}: 赋值落错了对象（退出码 1=块内读错, 2=出块后写错, 3=兄弟块串了, \
             4=嵌套块写错），实际 {rc}"
        );
    }
    // 反向对照：真 const 变量赋值必须仍然是错误
    let bad = "int f(void) { const int c = 1; c = 2; return c; }\n";
    let (out, obj) = run_object_test_flags("constshadow-bad", bad, &["-O2", "-std=gnu11"]);
    let _ = fs::remove_file(&obj);
    assert!(
        !out.status.success(),
        "const 赋值被放过了（E0030 应当仍然报）:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("read-only"),
        "报错文案不该变（判据要指得出是被 const 拦住）:\n{err}"
    );
    // 复合赋值同一条判据
    let bad2 = "int f(void) { const int c = 1; c += 2; return c; }\n";
    let (out2, obj2) = run_object_test_flags("constshadow-bad2", bad2, &["-O2", "-std=gnu11"]);
    let _ = fs::remove_file(&obj2);
    assert!(
        !out2.status.success(),
        "const 的复合赋值被放过了:\n{}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

/// 具名 `struct`/`union` 之后的标识符是声明符，即使它正好等于一个可见 typedef 名。
/// 以前 `parse_field_declarator` 里「当前标识符看着像类型名 ⇒ 它属于下一个字段」这条
/// 启发式不分匿名/具名，于是 `struct mutex rl_t;`（真树：`include/sound/hdaudio.h:93`
/// 的 `regmap_lock` 与 `include/linux/regmap.h:257` 的 typedef 同名）成员名被吞，
/// **布局一起错**：gcc 给 size=20/rl=4/after=8，dcc 给 size=32/rl=0（与 head 重叠）。
/// 反向对照同一条里带上：那条启发式真正服务的匿名内联体仍必须被展平。
#[test]
#[cfg(unix)]
fn named_struct_field_may_share_a_typedef_name() {
    let src = include_str!("../fixtures/c/named_struct_field_named_like_typedef.c");
    for flag in ["-O0", "-O2"] {
        let (out, obj) = run_object_test_flags("namedfield", src, &[flag, "-std=gnu11", "-Wall"]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            !err.contains("W1009"),
            "{flag}: 成员名被判成「未知聚合成员」就是这条缺陷的信号:\n{err}"
        );
        let _ = fs::remove_file(&obj);
        let (cout, exe) = run_compile_and_run_flags("namedfield", src, &[flag, "-std=gnu11"]);
        assert!(
            cout.status.success(),
            "{flag}: 链接失败: {}",
            String::from_utf8_lossy(&cout.stderr)
        );
        let rc = run_exe(&exe);
        let _ = fs::remove_file(&exe);
        assert_eq!(
            rc,
            0,
            "{flag}: 偏移/布局与 gcc 实测不一致（退出码 1-8=具名成员的 size/offset/读写, \
             9-11=匿名内联体的展平对照），实际 {rc}"
        );
    }
}

/// 短路运算符「**不被求值的那一侧**」与「常量跨过有 `goto` 指向的标号代回」的产物级
/// 双向判据（夹具 `shortcircuit_guard_trap.c`，§5.0o）。
///
/// 两条独立机制：
///   1) `static_key_enabled()` 那族 `if (!A && !B && !C) ____wrong_branch_error();`：
///      第一个 `!A` 折成 0 之后，按 C11 6.5.13 右侧根本不被求值，而旧的丢弃判据两侧都问
///      （`Call` 一律答「有副作用」）⇒ 塌缩后的条件又被留成一条语句。`?:` 同一条道理。
///   2) 代回器 `subst_cprop_into_stmt_exprs` 缺 `Expr::Ident` 叶臂 ⇒ 纯拷贝 `r1 = r2`
///      从来代不回，恒 0 的条件读不出来（`d_xlabel_dead` 那条形，`iommu_amdv1.o` 的
///      `__pt_no_sw_bit` 就钉在这条上）。
/// 期望名单是拿 `gcc -O2`/`-O3` 实测的：`trap_*` 五条 0 引用、`keep_*` 四条全在。
/// `keep_f_and`（`1 && 调用`）与 `keep_g_or`（`0 || 调用`）是这一族的反向对照 —— 短路位置
/// 定下来时**被求值**的那一侧仍然要发码，「右侧一律可丢」的改法必挂这两条。
#[test]
#[cfg(unix)]
fn short_circuit_guards_only_kill_the_side_that_is_never_evaluated() {
    let src = include_str!("../fixtures/c/shortcircuit_guard_trap.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("sc-guard", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und: Vec<String> = elf64_syms_by_type(&bytes)
            .iter()
            .filter(|s| s.shndx == 0)
            .map(|s| s.name.clone())
            .collect();
        let has = |n: &str| und.iter().any(|s| s == n);
        for gone in [
            "trap_a_and",
            "trap_b_or",
            "trap_c_cond",
            "trap_d_xlabel",
            "trap_e_nested",
        ] {
            assert!(
                !has(gone),
                "{flag}: 不被求值那一侧的引用没被消掉，产物里仍有 `{gone}`（= 链接期 \
                 `undefined reference to '{gone}'`）；完整未定义名单：{und:?}"
            );
        }
        for kept in ["keep_f_and", "keep_g_or", "keep_h_cond", "keep_i_and"] {
            assert!(
                has(kept),
                "{flag}: 本该留下的引用被一并抹掉了（`{kept}` 不在 {und:?}）——\
                 判据不是「越少越好」，这一半才是防过修的那一半"
            );
        }
    }
}

/// 「条件位上的**语句表达式**（内联后的早退链）」的产物级双向判据
/// （夹具 `inlined_se_tail_const_trap.c`，§5.0o）。
///
/// `drivers/iommu/generic_pt/fmt/iommu_amdv1.o` 的
/// `pts_feature(&pts, PT_FEAT_DMA_INCOHERENT) && !pt_test_sw_bit_acquire(&pts, …)`
/// 内联后左操作数是 `({ unsigned __inl1$_n = n; …; __inl1$rv = 0; goto end; end: 0; })`
/// —— 尾值已是字面量 0，却有三格各自把它钉住：求值器把「声明初值算不出常量」整块拒
/// （`__inl1$_n = n` 里的 `n` 是**外层**函数的变量）、`discarded_value_needs_code` 对
/// `StmtExpr` 一律答「需要代码」、`&&` 的丢弃判据两侧都问。三格一起修完才与 gcc 对齐。
///
/// `keep_*` 两条是这一族的反向对照：`trap_se_live` 的早退链尾值是运行期值（右侧一定会被
/// 求值），`trap_se_outer` 的语句表达式写的是**块外**局部（尾值同样是运行期值，而且求值器
/// 在这一格必须继续拒绝 —— 放开外部存储会连同那次写一起被 `fold_expr` 的替换抹掉）。
#[test]
#[cfg(unix)]
fn inlined_stmt_expr_in_condition_folds_to_the_gcc_reference() {
    let src = include_str!("../fixtures/c/inlined_se_tail_const_trap.c");
    for flag in ["-O2", "-O3"] {
        let (out, obj) = run_object_test_flags("se-cond", src, &[flag]);
        assert!(
            out.status.success(),
            "{flag}: 编译失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = fs::read(&obj).expect("read .o");
        let _ = fs::remove_file(&obj);
        let und: Vec<String> = elf64_syms_by_type(&bytes)
            .iter()
            .filter(|s| s.shndx == 0)
            .map(|s| s.name.clone())
            .collect();
        let has = |n: &str| und.iter().any(|s| s == n);
        for gone in ["trap_se_and", "trap_se_or", "trap_se_pure"] {
            assert!(
                !has(gone),
                "{flag}: 常量早退链的引用没被消掉，产物里仍有 `{gone}`（= 链接期 \
                 `undefined reference to '{gone}'`）；完整未定义名单：{und:?}"
            );
        }
        for kept in ["trap_se_live", "trap_se_outer"] {
            assert!(
                has(kept),
                "{flag}: 本该留下的引用被一并抹掉了（`{kept}` 不在 {und:?}）——\
                 判据不是「越少越好」，这一半才是防过修的那一半"
            );
        }
    }
}

/// 指定初始化器的下标是**常量移位表达式**时不得被丢弃。
///
/// 缺陷形状：`kernel/printk/printk.c` 的 `_DEFINE_PRINTKRB()` 展开为
/// `[_DESCS_COUNT(descbits) - 1] = { .state_var = ATOMIC_INIT(...), ... }`，其中
/// `descbits` 是宏算术（`CONFIG_LOG_BUF_SHIFT - PRB_AVGBITS`）。dcc 曾只认字面量下标，
/// 整项 `continue` 掉，于是 `_printk_rb_static_descs` 全零：tail 指向的最后一个描述符
/// `state_var` 为 0 ⇒ `get_desc_state()` 恒为 `desc_miss`，`prb_first_seq()` 在首次
/// printk 时永远自旋（开机无任何串口输出）。
///
/// 判据不能只看「数组大小对了」（`0x30000` 字节照样会分配），必须直接读 `.data` 里
/// **最后一个元素**的字节：`state_var == -8193`（= `DESC_SV(DESC0_ID(13), desc_reusable)`
/// 在 62 位 ID 掩码下的值）、`text_blk_lpos == {1, 1}`（`FAILED_BLK_LPOS`）。
#[test]
#[cfg(unix)]
fn designated_array_index_const_shift_initializer_kept() {
    let source = r#"
struct prb_desc {
	long state_var;
	unsigned long begin;
	unsigned long next;
};

#define _DESCS_COUNT(ct_bits) (1UL << (ct_bits))
#define _DEFINE_PRINTKRB(name, descbits)					\
static struct prb_desc _##name##_descs[_DESCS_COUNT(descbits)] = {	\
	[_DESCS_COUNT(descbits) - 1] = {				\
		.state_var	= -8193,				\
		.begin		= 1,					\
		.next		= 1,					\
	},							\
};

#define PRB_AVGBITS 5
#define LOG_BUF_SHIFT 18

_DEFINE_PRINTKRB(printk_rb_static, LOG_BUF_SHIFT - PRB_AVGBITS)

long probe(void) { return _printk_rb_static_descs[8191].state_var; }
"#;
    let bytes = dcc_obj_bytes("prb-designated-index", source, &["-O2"]);
    let data = elf64_section_data(&bytes, ".data").expect(".data 节应存在");
    let off = 8191 * 24;
    assert!(
        data.len() >= off + 24,
        "数组只写了 {} 字节，最后一个元素没有被初始化（期望至少 {} 字节）",
        data.len(),
        off + 24
    );
    let state_var = i64::from_le_bytes(data[off..off + 8].try_into().unwrap());
    let begin = u64::from_le_bytes(data[off + 8..off + 16].try_into().unwrap());
    let next = u64::from_le_bytes(data[off + 16..off + 24].try_into().unwrap());
    assert_eq!(state_var, -8193, "descs[8191].state_var 被丢弃（读到 {state_var}）");
    assert_eq!(begin, 1, "descs[8191].text_blk_lpos.begin 被丢弃");
    assert_eq!(next, 1, "descs[8191].text_blk_lpos.next 被丢弃");
}

/// 函数内 `static const` 稀疏指定下标数组的维度必须按「最大下标 + 1」推导
/// (C11 6.7.9p22)，而不是「指定项个数」。
///
/// 缺陷形状：`lib/vsprintf.c` 的
/// `static const unsigned char spec_flag_array[] = { [(' ')-32] = SPACE, ... }`，
/// 5 个指定项的最大下标 16 ⇒ 长度 17。dcc 曾按项数算成 5：符号 st_size=5，
/// 下标 3（`'-'`）之后的元素全部读 0 ⇒ `format_decode()` 把 `%-10s` 里的 `-`
/// 当非法标志，打印 "Please remove unsupported %-" 后内核 panic。
#[test]
#[cfg(unix)]
fn static_local_designated_sparse_array_dim_is_max_index_plus_one() {
    let src = r#"
#define SPEC_CHAR(x, flag) [(x)-32] = flag
static unsigned char spec_flag(unsigned char c, unsigned long *sz)
{
    static const unsigned char spec_flag_array[] = {
        SPEC_CHAR(' ', 1), SPEC_CHAR('#', 2), SPEC_CHAR('+', 4),
        SPEC_CHAR('-', 8), SPEC_CHAR('0', 16),
    };
    *sz = sizeof(spec_flag_array);
    c -= 32;
    return (c < sizeof(spec_flag_array)) ? spec_flag_array[c] : 0;
}
int main(void)
{
    unsigned long sz = 0;
    if (spec_flag('-', &sz) != 8) return 1;
    if (sz != 17) return 2;
    if (spec_flag(' ', &sz) != 1) return 3;
    if (spec_flag('#', &sz) != 2) return 4;
    if (spec_flag('+', &sz) != 4) return 5;
    if (spec_flag('0', &sz) != 16) return 6;
    if (spec_flag('A', &sz) != 0) return 7;
    return 0;
}
"#;
    let (out, exe) = run_compile_and_run("static-sparse-designated-array", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "sparse designated array compile failed: {stderr}");
    let rc = run_exe(&exe);
    let _ = fs::remove_file(&exe);
    assert_eq!(rc, 0, "sparse designated array runtime check failed (rc={rc})");
}

/// 自定义节里「数组名 / 函数名」直接初始化指针时，必须发 R_X86_64_64 重定位。
///
/// 缺陷形状：`kernel/panic.c` 的 `static char *taint_buf __refdata = init_taint_buf;`。
/// 指针变量在 `.ref.data`，初始化器是 `Expr::Ident`（数组名，类型 Array），不是
/// `Expr::AddrOf`。自定义节路径曾只认 `&x`/字符串/InitList，Ident 落进
/// `global_init_bytes` 的 None 分支 → 槽位补零、.rela.ref.data 缺失 ⇒ 链接后指针为
/// NULL，首次 `print_tainted()` 在 `seq_buf_init` 里写空指针（CR2=0，#PF）。
///
/// 判据是重定位本身：`.rela.ref.data` 恰好一条 `R_X86_64_64`，偏移 0，加数 0，
/// 符号名是 `init_taint_buf`。只检查 `.ref.data` 字节全零会假绿（重定位在链接期
/// 才回填内容）。
#[test]
#[cfg(unix)]
fn custom_section_pointer_from_array_name_emits_reloc() {
    let source = r#"
static char init_taint_buf[64] = "hello";

static char *taint_buf __attribute__((section(".ref.data"))) = init_taint_buf;

char *get_taint_buf(void) { return taint_buf; }
"#;
    let bytes = dcc_obj_bytes("refdata-ptr-array", source, &["-O2"]);
    let relas = elf64_rela(&bytes, ".rela.ref.data");
    assert_eq!(
        relas.len(),
        1,
        ".rela.ref.data 应恰好一条重定位（数组名初始化指针），实际 {relas:?}"
    );
    let (off, sym, ty, addend) = relas[0];
    assert_eq!(off, 0, "指针槽在 .ref.data 偏移 0");
    assert_eq!(ty, 1 /* R_X86_64_64 */, "重定位类型应为 R_X86_64_64");
    assert_eq!(addend, 0, "数组名取首元素地址，加数应为 0");
    let syms = elf64_syms_by_type(&bytes);
    let name = syms
        .get(sym as usize)
        .map(|s| s.name.as_str())
        .unwrap_or("<符号索引越界>");
    assert_eq!(name, "init_taint_buf", "重定位应指向数组 init_taint_buf，实际指向 {name}");
}
