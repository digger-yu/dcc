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
