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
