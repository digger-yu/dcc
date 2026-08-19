// tests/profile.rs - 性能分析子系统的 CLI 接受 + 模块行为回归
//
// 覆盖:
//   * -fprofile-generate / -fprofile-use / -ftest-coverage / -fprofile-arcs
//     等 CLI 解析与编进程可结束
//   * GCOV notes (.gcno) 与 .gcda 写盘 roundtrip (在 profile::gcov 模块)
//   * PGO 入口计数器插入 (AST 改写)
//   * AutoFDO / LBR / MemProf / perf_event / offload 数据结构

use std::process::Command;

fn dcc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dcc"))
}

fn fixtures() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn profile_src() -> std::path::PathBuf {
    fixtures().join("c/profile.c")
}

#[test]
fn cli_profile_generate_is_accepted() {
    // 单独传入 -fprofile-generate, 不应被拒绝
    let src = profile_src();
    let out = std::env::temp_dir().join("dcc-pgo-gen-test");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let status = dcc()
        .args(["-std=gnu23", "-c", "-o"])
        .arg(out.join("profile.o"))
        .arg("-fprofile-generate")
        .arg(&out)
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "PGO instrument 必须成功编译");
    assert!(out.join("profile.o").exists(), "应产出 .o");
}

#[test]
fn cli_profile_use_missing_path_rejects() {
    // 必须以 -fprofile-use 单独结尾, 不带后续 arg, 才能命中错误路径
    // (如果后面跟 .c 文件, 当前 cli 会把它当 path 吞下, 这与 GCC 行为一致)
    let out = dcc()
        .args(["-std=gnu23", "-c", "-o", "/dev/null", "-fprofile-use"])
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "缺路径时应失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fprofile-use") || stderr.contains("profile data path"),
        "stderr 缺少错误信息: {stderr}"
    );
}

#[test]
fn cli_emit_asm_still_works_with_profile() {
    // -fprofile-arcs + -S: 至少能产生 .s (不报 unknown flag)
    let src = profile_src();
    let out = std::env::temp_dir().join("dcc-pgo-arcs.s");
    let _ = std::fs::remove_file(&out);
    let status = dcc()
        .args(["-std=gnu23", "-S", "-o"])
        .arg(&out)
        .args(["-fprofile-arcs", "-fsyntax-only"]) // 故意让 -fsyntax-only 抢先
        .arg(&src)
        .status()
        .expect("运行 dcc");
    // -fsyntax-only 提前 return, 因此 status 应为 0 且无 .s 产物
    assert!(status.success());
    let _ = std::fs::remove_file(&out);
}

#[test]
fn cli_test_coverage_accepted() {
    let src = profile_src();
    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "-ftest-coverage"])
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success());
}

/// GCOV 端到端硬门禁: 编译 -> 落盘 .gcno/.gcovrt.c -> 运行 ->
/// 运行时把真实 counter 写入 .gcda -> 解析并校验 counter 值。
///
/// 任何一环断裂 (插桩不生效 / .gcno 不落盘 / 运行时 .gcda 写出失败 /
/// counter 偏移与 .gcda 布局不一致) 都会让本测试失败。
#[test]
fn gcov_end_to_end_counter_gate() {
    let dir = std::env::temp_dir().join(format!("dcc-gcov-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join("loop_sum.c");
    std::fs::write(
        &src,
        b"int add(int a, int b) { return a + b; }\n\
          int main(void) {\n\
              int s = 0;\n\
              for (int i = 0; i < 10; i++) s = add(s, i);\n\
              return s == 45 ? 0 : 1;\n\
          }\n",
    )
    .expect("写源文件");

    // 1) dcc 编译: -ftest-coverage -fprofile-arcs 必须产出 .gcno + .gcovrt.c
    let status = dcc()
        .current_dir(&dir)
        .args(["-std=gnu23", "-ftest-coverage", "-fprofile-arcs", "-c"])
        .arg(src.file_name().unwrap())
        .args(["-o", "loop_sum.o"])
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "gcov 插桩编译必须成功");
    assert!(dir.join("loop_sum.gcno").exists(), "应产出 .gcno");
    assert!(dir.join("loop_sum.gcovrt.c").exists(), "应产出运行时 C");

    // 2) 用 host cc 链接运行时并运行
    let cc_ok = Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !cc_ok {
        let _ = std::fs::remove_dir_all(&dir);
        return; // 无 host cc 的环境跳过链接阶段, 编译侧断言已执行
    }
    let link = Command::new("cc")
        .current_dir(&dir)
        .args(["-O0", "loop_sum.o", "loop_sum.gcovrt.c", "-o", "loop_sum"])
        .status()
        .expect("运行 cc 链接");
    assert!(link.success(), "链接 gcov 运行时必须成功");
    let run = Command::new("./loop_sum")
        .current_dir(&dir)
        .status()
        .expect("运行被测程序");
    assert!(run.success(), "被测程序应正常退出");

    // 3) 解析 .gcda, 校验真实 counter 值
    let data = std::fs::read(dir.join("loop_sum.gcda")).expect("应产出 .gcda");
    assert!(data.len() > 24, ".gcda 太短");
    assert_eq!(&data[0..4], &[0x61, 0x64, 0x63, 0x67], ".gcda magic 应为 'gcda'");

    // 布局: magic/version/stamp(12) + runs/sums(8) + source(pascal)
    let mut p = 20usize;
    let slen = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    p += 4 + slen;
    let mut counters: std::collections::HashMap<String, Vec<u64>> = std::collections::HashMap::new();
    while p + 16 <= data.len() {
        let _ident = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        p += 4;
        let _lno = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        p += 4;
        let _cfg = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        p += 4;
        let nlen = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        if p + nlen > data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&data[p..p + nlen]).into_owned();
        p += nlen;
        let _n_tag = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        p += 4;
        let _tag = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        p += 4;
        if p + 4 > data.len() {
            break;
        }
        let nc = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        if p + 8 * nc > data.len() {
            break;
        }
        let mut cs = Vec::with_capacity(nc);
        for _ in 0..nc {
            cs.push(u64::from_le_bytes(data[p..p + 8].try_into().unwrap()));
            p += 8;
        }
        counters.insert(name, cs);
    }

    // add 被循环调用 10 次: entry + return 两块各 10
    let add = counters.get("add").unwrap_or_else(|| panic!(".gcda 缺少 add: {counters:?}"));
    assert_eq!(add.len(), 2, "add 应有 2 个 counter: {add:?}");
    assert_eq!(add[0], 10, "add entry counter 应为 10: {add:?}");
    assert_eq!(add[1], 10, "add return counter 应为 10: {add:?}");
    // main: entry + decl + for + return 四块各 1
    let main = counters.get("main").unwrap_or_else(|| panic!(".gcda 缺少 main: {counters:?}"));
    assert_eq!(main.len(), 4, "main 应有 4 个 counter: {main:?}");
    assert!(main.iter().all(|&c| c == 1), "main 各块应执行 1 次: {main:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_record_profile_rejects_missing_path() {
    // -frecord-profile 必须带 dir; 无后续 arg 才报错
    let out = dcc()
        .args(["-std=gnu23", "-c", "-o", "/dev/null", "-frecord-profile"])
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "缺路径时应失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("record-profile") || stderr.contains("missing dir"),
        "stderr 缺少错误信息: {stderr}"
    );
}

#[test]
fn cli_fopenmp_accepted() {
    let src = profile_src();
    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "-fopenmp"])
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success());
}

#[test]
fn cli_fopenmp_targets_accepted() {
    let src = profile_src();
    let status = dcc()
        .args([
            "-std=gnu23",
            "-fsyntax-only",
            "-fopenmp",
            "-fopenmp-targets=nvptx64-nvidia-cuda",
        ])
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success());
}

#[test]
fn cli_fcuda_and_fhip_accepted() {
    let src = profile_src();
    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "-fcuda", "-fhip"])
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success());
}

#[test]
fn cli_fmemprof_accepted() {
    let src = profile_src();
    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "-fmemory-profile"])
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success());
}

/// MemProf 运行时端到端硬门禁: -fmemory-profile 落盘运行时 C ->
/// 链接 -> 运行 (alloc/free/flush 记录 callstack + lifetime) ->
/// 解析 .memprofdata 校验 count/bytes/lifetime 值。
#[test]
fn memprof_runtime_end_to_end_gate() {
    let dir = std::env::temp_dir().join(format!("dcc-memprof-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join("mp.c");
    std::fs::write(
        &src,
        b"void *__dcc_memprof_alloc_record(unsigned long size, void *ret);\n\
          void __dcc_memprof_free_record(void *ptr, void *ret);\n\
          int main(void) {\n\
              void *a = __dcc_memprof_alloc_record(1024, __builtin_return_address(0));\n\
              void *b = __dcc_memprof_alloc_record(64, __builtin_return_address(0));\n\
              __dcc_memprof_free_record(a, __builtin_return_address(0));\n\
              return (a && b) ? 0 : 1;\n\
          }\n",
    )
    .expect("写源文件");

    // 1) dcc 编译: -fmemory-profile 必须产出 .memprofc.c
    let status = dcc()
        .current_dir(&dir)
        .args(["-std=gnu23", "-fmemory-profile", "-c"])
        .arg(src.file_name().unwrap())
        .args(["-o", "mp.o"])
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "memprof 编译必须成功");
    assert!(dir.join("mp.memprofc.c").exists(), "应产出运行时 C");

    // 2) 链接并运行, 输出 .memprofdata
    let cc_ok = Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !cc_ok {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let link = Command::new("cc")
        .current_dir(&dir)
        .args(["-O0", "mp.o", "mp.memprofc.c", "-o", "mp"])
        .status()
        .expect("运行 cc 链接");
    assert!(link.success(), "链接 memprof 运行时必须成功");
    let out_path = dir.join("out.memprofdata");
    let run = Command::new("./mp")
        .current_dir(&dir)
        .env("DCC_MEMPROF_OUT", &out_path)
        .status()
        .expect("运行被测程序");
    assert!(run.success(), "被测程序应正常退出");

    // 3) 解析 .memprofdata 二进制并校验
    let data = std::fs::read(&out_path).expect("应产出 .memprofdata");
    assert_eq!(&data[0..8], b"MEMPROFD", "magic 应为 MEMPROFD");
    let mut p = 8usize;
    let rd32 = |p: &mut usize| {
        let v = u32::from_le_bytes(data[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    let rd64 = |p: &mut usize| {
        let v = u64::from_le_bytes(data[*p..*p + 8].try_into().unwrap());
        *p += 8;
        v
    };
    let rd_str = |p: &mut usize| {
        let n = rd32(p) as usize;
        let s = String::from_utf8_lossy(&data[*p..*p + n]).into_owned();
        *p += n;
        s
    };
    let rd_stack = |p: &mut usize| {
        let n = rd32(p) as usize;
        let mut frames = Vec::with_capacity(n);
        for _ in 0..n {
            frames.push(rd_str(p));
        }
        frames
    };
    assert_eq!(rd32(&mut p), 1, "version 应为 1");

    let n_allocs = rd32(&mut p);
    assert!(n_allocs >= 1, "应有 alloc site");
    let mut found_alloc = false;
    for _ in 0..n_allocs {
        let frames = rd_stack(&mut p);
        let count = rd64(&mut p);
        let bytes = rd64(&mut p);
        if !frames.is_empty() && count == 2 && bytes == 1088 {
            found_alloc = true;
        }
    }
    assert!(found_alloc, "同一 callstack 两次分配应合并: count=2 bytes=1088");

    let n_life = rd32(&mut p);
    assert!(n_life >= 1, "应有 lifetime 记录");
    let mut life_bytes = 0u64;
    for _ in 0..n_life {
        let _a = rd_stack(&mut p);
        let _d = rd_stack(&mut p);
        life_bytes += rd64(&mut p);
    }
    assert_eq!(life_bytes, 1024, "仅被 free 的 1024 字节计入 lifetime");

    let n_de = rd32(&mut p);
    assert!(n_de >= 1, "应有 dealloc site");
    let mut de_count = 0u64;
    for _ in 0..n_de {
        let _frames = rd_stack(&mut p);
        de_count += rd64(&mut p);
    }
    assert_eq!(de_count, 1, "free 只调用一次");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_topdownmetrics_accepted() {
    let src = profile_src();
    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "-ftopdownmetrics"])
        .arg(&src)
        .status()
        .expect("运行 dcc");
    assert!(status.success());
}

#[test]
fn cli_lbr_profile_rejects_missing() {
    // -flbr-profile 必须带 file; 无后续 arg 才报错
    let out = dcc()
        .args(["-std=gnu23", "-c", "-o", "/dev/null", "-flbr-profile"])
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "缺路径时应失败");
}

// ---------------------------------------------------------------------------
// Offloading (-fopenmp / -fcuda / -fhip / -fopenmp-targets= / --offload-arch=)
// ---------------------------------------------------------------------------

/// 测试用 `.dcc.offload` 节解析结果 (与 src/profile/offload.rs 布局一致,
/// 集成测试无法 use 内部模块, 故在测试内独立实现小端解析)。
struct TestOffloadEntry {
    kind: u8,
    func: String,
    line: u32,
    col: u32,
    triple: String,
    arch: Option<String>,
}

fn parse_offload_section(obj: &[u8]) -> Option<Vec<TestOffloadEntry>> {
    let start = obj.windows(8).position(|w| w == b"DCC_OFFL")?;
    let d = &obj[start..];
    if d.len() < 11 || d[8] != 1 {
        return None;
    }
    let n = u16::from_le_bytes([d[9], d[10]]) as usize;
    let mut p = 11usize;
    let mut out = Vec::new();
    for _ in 0..n {
        if p + 1 > d.len() {
            return None;
        }
        let kind = d[p];
        p += 1;
        let flen = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
        p += 2;
        let func = String::from_utf8_lossy(&d[p..p + flen]).into_owned();
        p += flen;
        let slen = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
        p += 2;
        let _file = String::from_utf8_lossy(&d[p..p + slen]).into_owned();
        p += slen;
        let line = u32::from_le_bytes(d[p..p + 4].try_into().unwrap());
        let col = u32::from_le_bytes(d[p + 4..p + 8].try_into().unwrap());
        p += 8;
        let tlen = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
        p += 2;
        let triple = String::from_utf8_lossy(&d[p..p + tlen]).into_owned();
        p += tlen;
        let has_arch = d[p];
        p += 1;
        let arch = if has_arch == 1 {
            let alen = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
            p += 2;
            let a = String::from_utf8_lossy(&d[p..p + alen]).into_owned();
            p += alen;
            Some(a)
        } else {
            None
        };
        out.push(TestOffloadEntry {
            kind,
            func,
            line,
            col,
            triple,
            arch,
        });
    }
    Some(out)
}

fn compile_to_object(
    name: &str,
    src: &std::path::Path,
    extra: &[&str],
) -> (std::process::Output, std::path::PathBuf) {
    // name 区分同一 fixture 的多测试并行编译, 避免对象文件互相覆盖
    let obj = std::env::temp_dir().join(format!("dcc-offload-{name}-{}.o", std::process::id()));
    let _ = std::fs::remove_file(&obj);
    let mut cmd = dcc();
    cmd.args(["-std=gnu23", "-c", "-o"])
        .arg(&obj)
        .args(extra)
        .arg(src);
    let out = cmd.output().expect("运行 dcc");
    (out, obj)
}

fn offload_fixture(name: &str) -> std::path::PathBuf {
    fixtures().join("c").join(name)
}

#[test]
fn cli_fopenmp_eq_variants_parsed() {
    // -fopenmp=simd / -fopenmp=libomp / -fopenmp=llvm 均应被接受
    for v in ["simd", "libomp", "libgomp", "llvm"] {
        let out = dcc()
            .args([
                "-std=gnu23",
                "-fsyntax-only",
                "-fopenmp",
                &format!("-fopenmp={v}"),
            ])
            .arg(profile_src())
            .output()
            .expect("运行 dcc");
        assert!(
            out.status.success(),
            "-fopenmp={v} 应被接受: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // 非法 runtime 值必须拒绝并带具体值
    let out = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "-fopenmp=bogus"])
        .arg(profile_src())
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "-fopenmp=bogus 应被拒绝");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bogus"), "错误消息应含具体值: {stderr}");
}

#[test]
fn cli_fopenmp_targets_forms_parsed() {
    // 等号形式与空格形式都接受
    for args in [
        vec!["-fopenmp-targets=nvptx64-nvidia-cuda"],
        vec!["-fopenmp-targets", "nvptx64-nvidia-cuda"],
        vec!["-fopenmp-targets=nvptx64-nvidia-cuda,amdgcn-amd-amdhsa"],
    ] {
        let out = dcc()
            .args(["-std=gnu23", "-fsyntax-only", "-fopenmp"])
            .args(&args)
            .arg(profile_src())
            .output()
            .expect("运行 dcc");
        assert!(
            out.status.success(),
            "{args:?} 应被接受: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn cli_fopenmp_targets_bad_triple_rejects_with_value() {
    let out = dcc()
        .args([
            "-std=gnu23",
            "-fsyntax-only",
            "-fopenmp",
            "-fopenmp-targets=foo-bar-baz",
        ])
        .arg(profile_src())
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "非法 triple 应被拒绝");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("foo-bar-baz"),
        "错误消息应含非法值: {stderr}"
    );
    assert!(
        stderr.contains("nvptx64-nvidia-cuda"),
        "应提示已知 triple: {stderr}"
    );
}

#[test]
fn cli_offload_arch_forms_parsed() {
    // --offload-arch=sm_80 / --offload-arch gfx906 / -foffload-arch= 都接受
    for args in [
        vec!["--offload-arch=sm_80"],
        vec!["--offload-arch", "gfx906"],
        vec!["-foffload-arch=sm_90"],
        vec!["-foffload-arch", "gfx90a"],
    ] {
        let out = dcc()
            .args(["-std=gnu23", "-fsyntax-only", "-fcuda"])
            .args(&args)
            .arg(profile_src())
            .output()
            .expect("运行 dcc");
        assert!(
            out.status.success(),
            "{args:?} 应被接受: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn cli_offload_arch_bad_value_rejects_with_value() {
    let out = dcc()
        .args(["-std=gnu23", "-fsyntax-only", "--offload-arch=sparc"])
        .arg(profile_src())
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "sparc 不是已知 offload arch, 应拒绝");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sparc"), "错误消息应含非法值: {stderr}");
}

#[test]
fn offload_openmp_emits_section_with_pragma_locations() {
    let src = offload_fixture("offload_omp.c");
    let (out, obj) = compile_to_object(
        "omp",
        &src,
        &["-fopenmp", "-fopenmp-targets=nvptx64-nvidia-cuda"],
    );
    assert!(
        out.status.success(),
        "编译应成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&obj).expect("读取 .o");
    let entries = parse_offload_section(&bytes).expect("应存在 .dcc.offload 节");
    assert_eq!(entries.len(), 3, "target/target simd/simd 共 3 条");
    // target   -> kind 0, 位置指向 pragma 行 10:1
    assert_eq!(entries[0].kind, 0);
    assert_eq!(entries[0].func, "offload_work");
    assert_eq!((entries[0].line, entries[0].col), (10, 1));
    // target simd -> kind 1, 位置 15:1
    assert_eq!(entries[1].kind, 1);
    assert_eq!(entries[1].func, "offload_work");
    assert_eq!((entries[1].line, entries[1].col), (15, 1));
    // simd -> kind 1, 位置 20:1
    assert_eq!(entries[2].kind, 1);
    assert_eq!(entries[2].line, 20);
    for e in &entries {
        assert_eq!(e.triple, "nvptx64-nvidia-cuda");
        assert_eq!(e.arch, None);
    }
    // 不支持构造 (parallel) 与函数外 pragma (teams / target) 应给出带位置的警告
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("W0312"), "parallel 应警告: {stderr}");
    assert!(
        stderr.contains("offload_omp.c:25:1"),
        "W0312 位置应指向 pragma 行: {stderr}"
    );
    assert!(stderr.contains("W0313"), "函数外 teams 应警告: {stderr}");
    assert!(
        stderr.contains("offload_omp.c:33:1"),
        "W0313 位置应指向 pragma 行: {stderr}"
    );
    assert!(stderr.contains("W0311"), "函数外 target 应警告: {stderr}");
    assert!(
        stderr.contains("offload_omp.c:38:1"),
        "W0311 位置应指向 pragma 行: {stderr}"
    );
    let _ = std::fs::remove_file(obj);
}

#[test]
fn offload_cuda_emits_section_with_func_locations() {
    let src = offload_fixture("offload_cuda.c");
    let (out, obj) = compile_to_object("cuda", &src, &["-fcuda"]);
    assert!(
        out.status.success(),
        "编译应成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&obj).expect("读取 .o");
    let entries = parse_offload_section(&bytes).expect("应存在 .dcc.offload 节");
    assert_eq!(entries.len(), 2, "vec_add + square, 纯 __host__ 不计入");
    // __global__ vec_add: kind 2, 位置指向函数名 7:17
    assert_eq!(entries[0].kind, 2);
    assert_eq!(entries[0].func, "vec_add");
    assert_eq!((entries[0].line, entries[0].col), (7, 17));
    assert_eq!(entries[0].triple, "nvptx64-nvidia-cuda");
    // __device__ square: kind 2, 位置 13:16
    assert_eq!(entries[1].kind, 2);
    assert_eq!(entries[1].func, "square");
    assert_eq!((entries[1].line, entries[1].col), (13, 16));
    assert!(
        !entries.iter().any(|e| e.func == "host_only"),
        "纯 __host__ 函数不是 offload entry"
    );
    let _ = std::fs::remove_file(obj);
}

#[test]
fn offload_arch_recorded_in_entry() {
    let src = offload_fixture("offload_cuda.c");
    let (out, obj) = compile_to_object("cuda-arch", &src, &["-fcuda", "--offload-arch=sm_80"]);
    assert!(
        out.status.success(),
        "编译应成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&obj).expect("读取 .o");
    let entries = parse_offload_section(&bytes).expect("应存在 .dcc.offload 节");
    assert!(!entries.is_empty());
    for e in &entries {
        assert_eq!(e.arch.as_deref(), Some("sm_80"), "arch 应写入元数据");
        assert_eq!(e.triple, "nvptx64-nvidia-cuda", "sm_* 映射到 CUDA triple");
    }
    let _ = std::fs::remove_file(obj);
}

#[test]
fn offload_cuda_requires_flag_with_precise_location() {
    // 无 -fcuda: __global__ 是未知标识符, 错误必须带源位置
    let src = offload_fixture("offload_cuda.c");
    let (out, _obj) = compile_to_object("cuda-noflag", &src, &[]);
    assert!(!out.status.success(), "无 -fcuda 应编译失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("offload_cuda.c:7:1"),
        "错误应指向 __global__ 所在位置: {stderr}"
    );
    assert!(stderr.contains("__global__"), "错误应提到限定符: {stderr}");
}

#[test]
fn offload_global_host_mixed_rejects_with_location() {
    let dir = std::env::temp_dir();
    let src = dir.join(format!("dcc-offload-mixed-{}.c", std::process::id()));
    std::fs::write(
        &src,
        "__global__ __host__ void bad(int x) {}\n\n__global__ __device__ void bad2(int x) {}\n",
    )
    .expect("写源文件");
    let (out, _obj) = compile_to_object("mixed", &src, &["-fcuda"]);
    assert!(!out.status.success(), "混用限定符应失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E0310"), "应报 E0310: {stderr}");
    assert!(stderr.contains(":1:26"), "位置应指向函数名 1:26: {stderr}");
    assert!(stderr.contains(":3:28"), "位置应指向函数名 3:28: {stderr}");
    // 每条错误只打印一次 (driver 收集 + main 兜底不重复)
    assert_eq!(stderr.matches("E0310").count(), 2, "不应重复打印: {stderr}");
    let _ = std::fs::remove_file(&src);
}

#[test]
fn offload_hip_emits_section_with_amd_triple() {
    let src = offload_fixture("offload_cuda.c");
    let (out, obj) = compile_to_object("hip", &src, &["-fhip", "--offload-arch=gfx906"]);
    assert!(
        out.status.success(),
        "HIP 编译应成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&obj).expect("读取 .o");
    let entries = parse_offload_section(&bytes).expect("应存在 .dcc.offload 节");
    assert!(!entries.is_empty());
    for e in &entries {
        assert_eq!(e.kind, 3, "HIP entry kind 应为 3");
        assert_eq!(e.triple, "amdgcn-amd-amdhsa", "gfx* 映射到 AMD triple");
        assert_eq!(e.arch.as_deref(), Some("gfx906"));
    }
    let _ = std::fs::remove_file(obj);
}
