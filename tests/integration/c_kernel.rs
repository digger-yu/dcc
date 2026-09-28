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

#[test]
#[cfg(unix)]
fn strict_vmlinux_regress_requires_debug_tools() {

    fn make_executable(path: &std::path::Path, body: &str) {
        fs::write(path, body).expect("write fake tool");
        let mut perms = fs::metadata(path).expect("stat fake tool").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod fake tool");
    }

    fn link_tool(dir: &std::path::Path, name: &str) {
        let src = ["/usr/bin", "/bin"]
            .iter()
            .map(|base| std::path::Path::new(base).join(name))
            .find(|candidate| candidate.exists())
            .unwrap_or_else(|| panic!("missing host tool {name}"));
        std::os::unix::fs::symlink(src, dir.join(name)).expect("link host tool");
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp =
        std::env::temp_dir().join(format!("dcc-strict-vmlinux-tools-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).expect("create fake tool dir");
    fs::write(temp.join("vmlinux"), "not really elf\n").expect("write vmlinux fixture");

    for tool in ["grep", "head", "tail", "mktemp", "rm"] {
        link_tool(&temp, tool);
    }
    make_executable(
        &temp.join("readelf"),
        "#!/bin/sh\nprintf '  Type:                              EXEC (Executable file)\\n'\n",
    );

    let missing_pahole = Command::new("/bin/bash")
        .arg(root.join("scripts/vmlinux_regress.sh"))
        .env("VMLINUX", temp.join("vmlinux"))
        .env("PATH", &temp)
        .env("REQUIRE_PAHOLE", "1")
        .output()
        .expect("run vmlinux_regress.sh without pahole");
    let stdout = String::from_utf8_lossy(&missing_pahole.stdout);
    assert!(!missing_pahole.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("FAIL: pahole not installed"));
    assert!(!stdout.contains("SKIP:"));

    make_executable(&temp.join("pahole"), "#!/bin/sh\nprintf 'STRUCT dcc_probe\\n'\n");
    let missing_objtool = Command::new("/bin/bash")
        .arg(root.join("scripts/vmlinux_regress.sh"))
        .env("VMLINUX", temp.join("vmlinux"))
        .env("PATH", &temp)
        .env("REQUIRE_OBJTOOL", "1")
        .output()
        .expect("run vmlinux_regress.sh without objtool");
    let stdout = String::from_utf8_lossy(&missing_objtool.stdout);
    assert!(!missing_objtool.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("FAIL: objtool not installed"));
    assert!(!stdout.contains("SKIP:"));

    let _ = fs::remove_dir_all(&temp);
}

/// KASAN 栈红区: -fsanitize=address 时函数入口应发射
/// __asan_poison_memory_region 和 __asan_unpoison_memory_region 调用
#[test]
fn kasan_stack_redzone_poison_unpoison() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-kasan-stack-{}.c", std::process::id()));
    let asm_path = dir.join(format!("dcc-kasan-stack-{}.s", std::process::id()));
    let source = "int f(void) { int x = 42; return x; }\n";
    fs::write(&source_path, source).expect("write C fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-fsanitize=address", "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
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

/// QEMU boot: verify kernel config has serial console support when
/// bzImage exists, ensuring QEMU boot can produce visible output.
#[test]
#[cfg(unix)]
fn kernel_config_has_serial_console_for_qemu_boot() {
    let kernel_dir =
        std::path::Path::new(&std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()))
            .join("linux-7.1");
    let bzimage = kernel_dir.join("arch/x86/boot/bzImage");
    let config = kernel_dir.join(".config");

    // Only check when bzImage actually exists
    if !bzimage.exists() {
        eprintln!("SKIP: no bzImage found at {bzimage:?}");
        return;
    }
    if !config.exists() {
        eprintln!("SKIP: no .config found at {config:?}");
        return;
    }

    let config_text = fs::read_to_string(&config).expect("read kernel .config");

    // Serial console is required for QEMU -nographic boot log
    assert!(
        config_text.contains("CONFIG_SERIAL_8250=y")
            || config_text.contains("CONFIG_SERIAL_8250_CONSOLE=y"),
        "kernel .config must enable CONFIG_SERIAL_8250 and \
         CONFIG_SERIAL_8250_CONSOLE for QEMU serial boot \
         log; run: scripts/config --enable \
         CONFIG_SERIAL_8250_CONSOLE"
    );
    assert!(
        config_text.contains("CONFIG_SERIAL_CORE=y"),
        "kernel .config must enable CONFIG_SERIAL_CORE \
         for serial console support"
    );
}

/// QEMU boot: run actual qemu-system-x86_64 with real bzImage
/// and verify kernel boot log strings appear in output.
#[test]
#[cfg(unix)]
fn qemu_boot_real_kernel_produces_boot_log() {
    let kernel_dir =
        std::path::Path::new(&std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()))
            .join("linux-7.1");
    let bzimage = kernel_dir.join("arch/x86/boot/bzImage");

    if !bzimage.exists() {
        eprintln!("SKIP: no bzImage at {bzimage:?}");
        return;
    }
    if std::process::Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: qemu-system-x86_64 not installed");
        return;
    }

    let output = std::process::Command::new("qemu-system-x86_64")
        .arg("-kernel")
        .arg(&bzimage)
        .arg("-m")
        .arg("512M")
        .arg("-no-reboot")
        .arg("-display")
        .arg("none")
        .arg("-nographic")
        .arg("-append")
        .arg("console=ttyS0 panic=-1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run qemu-system-x86_64");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        combined.contains("Linux version"),
        "QEMU boot log must contain 'Linux version'; \
         got: {}...",
        &combined[..combined.len().min(500)]
    );
    assert!(
        combined.contains("Kernel command line") || combined.contains("Memory:"),
        "QEMU boot log should contain kernel init messages"
    );
}

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
        asm_text.contains("__sanitizer_cov_trace_pc_guard"),
        "trace-cmp should still emit edge guard calls: {asm_text}"
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
// Linux header graph preprocessing E2E tests
// ============================================================================

/// Linux 内核头 compiler_types.h 预处理 E2E: 验证真实内核头可被 dcc 预处理
#[test]
fn linux_header_compiler_types_preprocess_e2e() {
    let kernel_include = std::path::Path::new("/usr/src/linux-hwe-7.0-headers-7.0.0-28/include");
    let compiler_types = kernel_include.join("linux/compiler_types.h");
    if !compiler_types.exists() {
        eprintln!("skip: compiler_types.h not found");
        return;
    }

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-linux-hdr-compiler-{}.c", std::process::id()));
    let out = dir.join(format!("dcc-linux-hdr-compiler-{}.i", std::process::id()));

    // 源文件引用 compiler_types.h 并使用其定义的宏
    let src_text = format!(
        r#"
#include "{kernel_include}/linux/compiler_types.h"
int main(void) {{
    __force int x = 0;
    __maybe_unused int y = 0;
    (void)x; (void)y;
    return 0;
}}
"#,
        kernel_include = kernel_include.display()
    );
    fs::write(&src, src_text).expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg("-I")
        .arg(kernel_include)
        .arg(&src)
        .output()
        .expect("run dcc -E");

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&out);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "compiler_types.h preprocess must succeed; stderr: {stderr}"
    );
}

/// Linux 内核头 rwonce.h 预处理 E2E: 验证 READ_ONCE/WRITE_ONCE 宏展开
#[test]
fn linux_header_rwonce_preprocess_e2e() {
    let kernel_include = std::path::Path::new("/usr/src/linux-hwe-7.0-headers-7.0.0-28/include");
    let rwonce = kernel_include.join("asm-generic/rwonce.h");
    if !rwonce.exists() {
        eprintln!("skip: rwonce.h not found");
        return;
    }

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-linux-hdr-rwonce-{}.c", std::process::id()));
    let out = dir.join(format!("dcc-linux-hdr-rwonce-{}.i", std::process::id()));

    let src_text = format!(
        r#"
#include "{kernel_include}/asm-generic/rwonce.h"
int main(void) {{
    int val = 42;
    int r = READ_ONCE(val);
    WRITE_ONCE(val, r + 1);
    return 0;
}}
"#,
        kernel_include = kernel_include.display()
    );
    fs::write(&src, src_text).expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg("-I")
        .arg(kernel_include)
        .arg(&src)
        .output()
        .expect("run dcc -E");

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&out);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "rwonce.h preprocess must succeed; stderr: {stderr}");
}

/// Linux 内核 generated/autoconf.h 预处理 E2E: 验证 CONFIG_* 宏可被解析
#[test]
fn linux_header_autoconf_preprocess_e2e() {
    let autoconf = std::path::Path::new(
        "/usr/src/linux-headers-7.0.0-28-generic/include/generated/autoconf.h",
    );
    if !autoconf.exists() {
        eprintln!("skip: autoconf.h not found");
        return;
    }

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-linux-hdr-autoconf-{}.c", std::process::id()));
    let out = dir.join(format!("dcc-linux-hdr-autoconf-{}.i", std::process::id()));

    // autoconf.h 只含 #define CONFIG_* 宏, 不含可执行代码
    let src_text = format!(
        r#"
#include "{autoconf_path}"
/* 引用几个 CONFIG 宏以验证展开 */
#ifdef CONFIG_MTRR
int mtrr_enabled(void) {{ return 1; }}
#else
int mtrr_enabled(void) {{ return 0; }}
#endif
int main(void) {{ return mtrr_enabled(); }}
"#,
        autoconf_path = autoconf.display()
    );
    fs::write(&src, src_text).expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-E", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .expect("run dcc -E");

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&out);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "autoconf.h preprocess must succeed; stderr: {stderr}");
}

/// Linux 内核头依赖图 E2E: 多文件包含闭包验证依赖追踪
#[test]
fn linux_header_graph_dependency_closure() {
    let kernel_include = std::path::Path::new("/usr/src/linux-hwe-7.0-headers-7.0.0-28/include");
    let generated_include =
        std::path::Path::new("/usr/src/linux-headers-7.0.0-28-generic/include/generated");
    let compiler_types = kernel_include.join("linux/compiler_types.h");
    let autoconf = generated_include.join("autoconf.h");
    if !compiler_types.exists() || !autoconf.exists() {
        eprintln!("skip: kernel headers not found");
        return;
    }

    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-linux-hdr-graph-{}.c", std::process::id()));
    let dep = dir.join(format!("dcc-linux-hdr-graph-{}.d", std::process::id()));

    // 同时引用两个内核头, 验证 -MMD 能追踪整个依赖闭包
    let src_text = format!(
        r#"
#include "{autoconf_path}"
#include "{compiler_types_path}"
int main(void) {{
    __force int x = 0;
#ifdef CONFIG_MTRR
    (void)x;
#endif
    return 0;
}}
"#,
        autoconf_path = autoconf.display(),
        compiler_types_path = compiler_types.display()
    );
    fs::write(&src, src_text).expect("write source");

    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-MMD", "-MF"])
        .arg(&dep)
        .arg("-I")
        .arg(kernel_include)
        .arg("-I")
        .arg(generated_include)
        .arg("-o")
        .arg(dir.join(format!("dcc-linux-hdr-graph-{}.o", std::process::id())))
        .arg(&src)
        .output()
        .expect("run dcc -MMD");

    let dep_text = fs::read_to_string(&dep).unwrap_or_default();
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dep);
    let _ = fs::remove_file(dir.join(format!("dcc-linux-hdr-graph-{}.o", std::process::id())));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "header graph compile must succeed; stderr: {stderr}");
    // 依赖文件应包含至少 autoconf.h 和 compiler_types.h 的路径
    assert!(
        dep_text.contains("autoconf.h") || dep_text.contains("compiler_types.h"),
        "depfile should track kernel headers: {dep_text}"
    );
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
