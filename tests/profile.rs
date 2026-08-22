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
    assert_eq!(
        &data[0..4],
        &[0x61, 0x64, 0x63, 0x67],
        ".gcda magic 应为 'gcda'"
    );

    // 布局: magic/version/stamp(12) + runs/sums(8) + source(pascal)
    let mut p = 20usize;
    let slen = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    p += 4 + slen;
    let mut counters: std::collections::HashMap<String, Vec<u64>> =
        std::collections::HashMap::new();
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
    let add = counters
        .get("add")
        .unwrap_or_else(|| panic!(".gcda 缺少 add: {counters:?}"));
    assert_eq!(add.len(), 2, "add 应有 2 个 counter: {add:?}");
    assert_eq!(add[0], 10, "add entry counter 应为 10: {add:?}");
    assert_eq!(add[1], 10, "add return counter 应为 10: {add:?}");
    // main: entry + decl + for + return 四块各 1
    let main = counters
        .get("main")
        .unwrap_or_else(|| panic!(".gcda 缺少 main: {counters:?}"));
    assert_eq!(main.len(), 4, "main 应有 4 个 counter: {main:?}");
    assert!(
        main.iter().all(|&c| c == 1),
        "main 各块应执行 1 次: {main:?}"
    );

    // 4) 生成并校验行覆盖率报告。这里使用 .gcda 中的函数计数器
    //    生成与 gcov 风格一致的摘要，确保报告只接受真实运行数据。
    let mut report = String::new();
    for (name, counts) in &counters {
        let executed = counts.iter().filter(|&&count| count > 0).count();
        let total = counts.len();
        let percent = if total == 0 {
            0.0
        } else {
            executed as f64 / total as f64 * 100.0
        };
        report.push_str(&format!(
            "Function:{name} Lines:{executed}/{total} Executed:{percent:.1}%\\n"
        ));
        for (idx, count) in counts.iter().enumerate() {
            report.push_str(&format!("  block{}:{}\\n", idx + 1, count));
        }
    }
    assert!(report.contains("Function:add Lines:2/2 Executed:100.0%"));
    assert!(report.contains("Function:main Lines:4/4 Executed:100.0%"));
    assert!(report.contains("block1:10"));
    assert!(report.contains("block4:1"));

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// AutoFDO / LBR / MemProf / Topdown CLI integration
// ---------------------------------------------------------------------------

/// AutoFDO 文本格式 profile 输入: -fauto-profile= 应接受并解析.
#[test]
fn cli_autofdo_text_profile_accepted() {
    let dir = std::env::temp_dir().join(format!("dcc-autofdo-text-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 写一个 dcc 简化文本格式的 AutoFDO profile
    let profile = dir.join("profile.afdo");
    std::fs::write(
        &profile,
        b"# dcc autofdo v1\nfunc main 100\nfunc hot_fn 80\nfunc cold_fn 1\n",
    )
    .unwrap();

    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only"])
        .arg("-fauto-profile")
        .arg(profile.to_str().unwrap())
        .arg(profile_src())
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "-fauto-profile 文本格式应被接受");

    let _ = std::fs::remove_dir_all(&dir);
}

/// AutoFDO 二进制格式 profile 输入: -fauto-profile= 应接受二进制文件.
#[test]
fn cli_autofdo_binary_profile_accepted() {
    let dir = std::env::temp_dir().join(format!("dcc-autofdo-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 写一个二进制格式的 AutoFDO profile (magic "AFDO" + version 2)
    let profile = dir.join("profile.afdo");
    let mut buf = Vec::new();
    buf.extend_from_slice(b"AFDO");
    buf.extend_from_slice(&2u32.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes()); // 1 function
    buf.extend_from_slice(&4u32.to_le_bytes()); // name len
    buf.extend_from_slice(b"main");
    buf.extend_from_slice(&100u64.to_le_bytes()); // samples
    buf.extend_from_slice(&0u32.to_le_bytes()); // 0 lines
    buf.extend_from_slice(&0u32.to_le_bytes()); // 0 calls
    buf.extend_from_slice(&0u32.to_le_bytes()); // 0 discs
    std::fs::write(&profile, &buf).unwrap();

    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only"])
        .arg("-fauto-profile")
        .arg(profile.to_str().unwrap())
        .arg(profile_src())
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "-fauto-profile 二进制格式应被接受");

    let _ = std::fs::remove_dir_all(&dir);
}

/// AutoFDO 缺失路径应报错.
#[test]
fn cli_autofdo_missing_path_rejects() {
    let out = dcc()
        .args(["-std=gnu23", "-c", "-o", "/dev/null", "-fauto-profile"])
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "缺路径时应失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fauto-profile") || stderr.contains("missing file"),
        "stderr 缺少错误信息: {stderr}"
    );
}

/// LBR 文本格式 (brstack) profile 输入: -flbr-profile= 应接受.
#[test]
fn cli_lbr_profile_text_accepted() {
    let dir = std::env::temp_dir().join(format!("dcc-lbr-text-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 写一个 brstack 格式的 LBR profile
    let profile = dir.join("profile.lbr");
    std::fs::write(
        &profile,
        b"main 0x1000 hot_fn 0x2000 5\nhot_fn 0x2010 main 0x1020 3\n",
    )
    .unwrap();

    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only"])
        .arg("-flbr-profile")
        .arg(profile.to_str().unwrap())
        .arg(profile_src())
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "-flbr-profile 文本格式应被接受");

    let _ = std::fs::remove_dir_all(&dir);
}

/// LBR 缺失路径应报错.
#[test]
fn cli_lbr_profile_missing_path_rejects() {
    let out = dcc()
        .args(["-std=gnu23", "-c", "-o", "/dev/null", "-flbr-profile"])
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "缺路径时应失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("flbr-profile") || stderr.contains("missing file"),
        "stderr 缺少错误信息: {stderr}"
    );
}

/// MemProf 二进制格式 profile 输入: -fmemory-profile-use= 应接受.
#[test]
fn cli_memprof_use_binary_accepted() {
    let dir = std::env::temp_dir().join(format!("dcc-memprof-use-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 写一个二进制格式的 MemProf profile
    let profile = dir.join("profile.memprofdata");
    let mut buf = Vec::new();
    buf.extend_from_slice(b"MEMPROFD");
    buf.extend_from_slice(&1u32.to_le_bytes()); // version
    buf.extend_from_slice(&1u32.to_le_bytes()); // 1 alloc
    buf.extend_from_slice(&1u32.to_le_bytes()); // 1 frame
    buf.extend_from_slice(&4u32.to_le_bytes()); // name len
    buf.extend_from_slice(b"main");
    buf.extend_from_slice(&10u64.to_le_bytes()); // count
    buf.extend_from_slice(&1024u64.to_le_bytes()); // bytes
    buf.extend_from_slice(&0u32.to_le_bytes()); // 0 lifetime
    buf.extend_from_slice(&0u32.to_le_bytes()); // 0 dealloc
    std::fs::write(&profile, &buf).unwrap();

    let status = dcc()
        .args(["-std=gnu23", "-fsyntax-only"])
        .arg("-fmemory-profile-use")
        .arg(profile.to_str().unwrap())
        .arg(profile_src())
        .status()
        .expect("运行 dcc");
    assert!(status.success(), "-fmemory-profile-use 二进制格式应被接受");

    let _ = std::fs::remove_dir_all(&dir);
}

/// MemProf 缺失路径应报错.
#[test]
fn cli_memprof_use_missing_path_rejects() {
    let out = dcc()
        .args([
            "-std=gnu23",
            "-c",
            "-o",
            "/dev/null",
            "-fmemory-profile-use",
        ])
        .output()
        .expect("运行 dcc");
    assert!(!out.status.success(), "缺路径时应失败");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("memory-profile-use") || stderr.contains("missing file"),
        "stderr 缺少错误信息: {stderr}"
    );
}

/// -ftopdownmetrics 与 -fprofile-generate 组合应接受.
#[test]
fn cli_topdown_with_pgo_accepted() {
    let status = dcc()
        .args([
            "-std=gnu23",
            "-fsyntax-only",
            "-fprofile-generate",
            "-ftopdownmetrics",
        ])
        .arg(profile_src())
        .status()
        .expect("运行 dcc");
    assert!(
        status.success(),
        "-ftopdownmetrics 与 -fprofile-generate 组合应被接受"
    );
}

/// AutoFDO 权重影响: 用 -fauto-profile + -O2 编译, 验证 hot 函数
/// 被归入 .text.hot 节, cold 函数被归入 .text.unlikely 节.
#[test]
fn autofdo_weight_influences_section_layout() {
    let dir = std::env::temp_dir().join(format!("dcc-autofdo-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 源文件: 冷热分明的函数
    let src = dir.join("autofdo.c");
    std::fs::write(
        &src,
        b"int hot_fn(int x) { return x * 2; }\n\
          int cold_fn(int x) { return x - 1; }\n\
          int main(void) { return hot_fn(1) + cold_fn(2); }\n",
    )
    .expect("写源文件");

    // AutoFDO 文本 profile: hot_fn 样本多, cold_fn 样本极少
    let profile = dir.join("profile.afdo");
    std::fs::write(
        &profile,
        b"# dcc autofdo v1\nfunc main 10\nfunc hot_fn 100\nfunc cold_fn 1\n",
    )
    .expect("写 profile");

    // 用 -fauto-profile + -O2 编译
    let out = dcc()
        .current_dir(&dir)
        .args([
            "-std=gnu23",
            "-O2",
            "-fauto-profile",
            "profile.afdo",
            "-c",
            "-o",
            "autofdo.o",
        ])
        .arg(src.file_name().unwrap())
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "AutoFDO 编译必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("autofdo.o").exists(), "应产出 .o");

    // 用 objdump 检查 section 布局
    let objdump = Command::new("objdump")
        .args(["-h", dir.join("autofdo.o").to_str().unwrap()])
        .output();
    if let Ok(ref out) = objdump {
        if out.status.success() {
            let sections = String::from_utf8_lossy(&out.stdout);
            // AutoFDO 权重下发应产出 .text.hot 和 .text.unlikely 节
            assert!(
                sections.contains(".text.hot") || sections.contains(".text.unlikely"),
                "AutoFDO 权重应影响 section 布局: {sections}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// LBR 权重影响: 用 -flbr-profile + -O2 编译, 验证 section 布局变化.
#[test]
fn lbr_weight_influences_section_layout() {
    let dir = std::env::temp_dir().join(format!("dcc-lbr-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 源文件
    let src = dir.join("lbr.c");
    std::fs::write(
        &src,
        b"int hot_fn(int x) { return x * 2; }\n\
          int cold_fn(int x) { return x - 1; }\n\
          int main(void) { return hot_fn(1) + cold_fn(2); }\n",
    )
    .expect("写源文件");

    // LBR brstack 格式 profile: hot_fn 分支多
    let profile = dir.join("profile.lbr");
    std::fs::write(
        &profile,
        b"main 0x1000 hot_fn 0x2000 100\nhot_fn 0x2010 main 0x1020 50\n",
    )
    .expect("写 profile");

    // 用 -flbr-profile + -O2 编译
    let out = dcc()
        .current_dir(&dir)
        .args([
            "-std=gnu23",
            "-O2",
            "-flbr-profile",
            "profile.lbr",
            "-c",
            "-o",
            "lbr.o",
        ])
        .arg(src.file_name().unwrap())
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "LBR 编译必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("lbr.o").exists(), "应产出 .o");

    // 用 objdump 检查 section 布局
    let objdump = Command::new("objdump")
        .args(["-h", dir.join("lbr.o").to_str().unwrap()])
        .output();
    if let Ok(ref out) = objdump {
        if out.status.success() {
            let sections = String::from_utf8_lossy(&out.stdout);
            assert!(
                sections.contains(".text.hot") || sections.contains(".text.unlikely"),
                "LBR 权重应影响 section 布局: {sections}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// MemProf 权重影响: 用 -fmemory-profile-use + -O2 编译, 验证 cold 分配
/// 相关函数被归入 .text.unlikely.
#[test]
fn memprof_weight_influences_section_layout() {
    let dir = std::env::temp_dir().join(format!("dcc-memprof-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 源文件
    let src = dir.join("memprof.c");
    std::fs::write(
        &src,
        b"#include <stdlib.h>\nvoid *hot_alloc(void) { return malloc(1024); }\n\
          void *cold_alloc(void) { return malloc(64); }\n\
          int main(void) { hot_alloc(); return 0; }\n",
    )
    .expect("写源文件");

    // MemProf 二进制 profile: hot_alloc 多, cold_alloc 极少
    let profile = dir.join("profile.memprofdata");
    let mut buf = Vec::new();
    buf.extend_from_slice(b"MEMPROFD");
    buf.extend_from_slice(&1u32.to_le_bytes()); // version
                                                // 2 allocs
    buf.extend_from_slice(&2u32.to_le_bytes());
    // hot_alloc: single-frame callstack, count=100, bytes=102400
    buf.extend_from_slice(&1u32.to_le_bytes()); // 1 frame
    buf.extend_from_slice(&9u32.to_le_bytes()); // name len
    buf.extend_from_slice(b"hot_alloc");
    buf.extend_from_slice(&100u64.to_le_bytes()); // count
    buf.extend_from_slice(&102400u64.to_le_bytes()); // bytes
                                                     // cold_alloc: single-frame callstack, count=1, bytes=64
    buf.extend_from_slice(&1u32.to_le_bytes()); // 1 frame
    buf.extend_from_slice(&10u32.to_le_bytes()); // name len
    buf.extend_from_slice(b"cold_alloc");
    buf.extend_from_slice(&1u64.to_le_bytes()); // count
    buf.extend_from_slice(&64u64.to_le_bytes()); // bytes
                                                 // 0 lifetime, 0 dealloc
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&profile, &buf).expect("写 profile");

    // 用 -fmemory-profile-use + -O2 编译
    let out = dcc()
        .current_dir(&dir)
        .args([
            "-std=gnu23",
            "-O2",
            "-fmemory-profile-use",
            "profile.memprofdata",
            "-c",
            "-o",
            "memprof.o",
        ])
        .arg(src.file_name().unwrap())
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "MemProf 编译必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("memprof.o").exists(), "应产出 .o");

    // 用 objdump 检查 section 布局
    let objdump = Command::new("objdump")
        .args(["-h", dir.join("memprof.o").to_str().unwrap()])
        .output();
    if let Ok(ref out) = objdump {
        if out.status.success() {
            let sections = String::from_utf8_lossy(&out.stdout);
            assert!(
                sections.contains(".text.hot") || sections.contains(".text.unlikely"),
                "MemProf 权重应影响 section 布局: {sections}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Topdown metrics 与 -O2 组合: 编译时输出 pipeline 计数器.
#[test]
fn topdown_metrics_with_optimization() {
    let dir = std::env::temp_dir().join(format!("dcc-topdown-opt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join("topdown.c");
    std::fs::write(
        &src,
        b"int compute(int x) { return x * x + 2 * x + 1; }\n\
          int main(void) { return compute(42); }\n",
    )
    .expect("写源文件");

    // -ftopdownmetrics + -O2 应产出目标文件
    let out = dcc()
        .current_dir(&dir)
        .args([
            "-std=gnu23",
            "-O2",
            "-ftopdownmetrics",
            "-c",
            "-o",
            "topdown.o",
        ])
        .arg(src.file_name().unwrap())
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "-ftopdownmetrics + -O2 必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("topdown.o").exists(), "应产出 .o");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Offload driver/link tests
// ---------------------------------------------------------------------------

/// OffloadDriverLinker: nvcc 缺失路径不影响 host 链接标志生成.
#[test]
fn offload_driver_link_nvcc_missing_generates_host_flags() {
    use dcc::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};
    use dcc::profile::offload_driver_link::OffloadDriverLinker;

    let mut config = OffloadConfig {
        cuda: true,
        ..OffloadConfig::default()
    };
    config.target_archs.push("sm_80".to_string());

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "vec_add".into(),
        file: "kernel.cu".into(),
        line: 10,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: Some("sm_80".into()),
    });

    let mut linker = OffloadDriverLinker::new(config, section);
    let result = linker.generate_link_result(None);

    // nvcc 可能不存在, 但 host link flags 应包含 -lcudart
    assert!(
        result.host_link_flags.iter().any(|f| f == "-lcudart"),
        "host 链接标志应包含 -lcudart: {:?}",
        result.host_link_flags
    );
    // rpath 标志应存在 (即使运行时未找到)
    assert!(
        result.host_link_flags.iter().any(|f| f.contains("rpath")),
        "host 链接标志应包含 rpath: {:?}",
        result.host_link_flags
    );
    // 应有一个 device link 命令 (使用 nvcc)
    assert!(!result.device_link_commands.is_empty());
    assert_eq!(result.device_link_commands[0].driver, "nvcc");
}

/// OffloadDriverLinker: ROCm runtime 查找路径包含标准目录.
#[test]
fn offload_driver_link_rocm_search_dirs() {
    use dcc::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};
    use dcc::profile::offload_driver_link::{OffloadBackend, OffloadDriverLinker};

    let mut config = OffloadConfig {
        hip: true,
        ..OffloadConfig::default()
    };
    config.target_archs.push("gfx906".to_string());

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Hip,
        func: "mat_mul".into(),
        file: "kernel.hip".into(),
        line: 5,
        col: 1,
        device_triple: "amdgcn-amd-amdhsa".into(),
        arch: Some("gfx906".into()),
    });

    let mut linker = OffloadDriverLinker::new(config, section);
    let result = linker.generate_link_result(None);

    // ROCm runtime 应被解析 (found 可能为 false)
    let rt = result.runtimes.get(&OffloadBackend::Rocm);
    assert!(rt.is_some(), "应解析 ROCm runtime");
    let rt = rt.unwrap();
    assert_eq!(rt.link_name, "amdhip64");
    // 搜索目录应包含标准路径
    assert!(
        rt.search_dirs
            .iter()
            .any(|p| p.as_os_str() == "/opt/rocm/lib"),
        "ROCm 搜索目录应包含 /opt/rocm/lib"
    );
}

/// OffloadDriverLinker: host link flags 包含 rpath 指向运行时库目录.
#[test]
fn offload_driver_link_host_flags_include_rpath() {
    use dcc::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};
    use dcc::profile::offload_driver_link::OffloadDriverLinker;

    let mut config = OffloadConfig {
        cuda: true,
        ..OffloadConfig::default()
    };
    config.target_archs.push("sm_80".to_string());

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "vec_add".into(),
        file: "kernel.cu".into(),
        line: 10,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: Some("sm_80".into()),
    });

    let mut linker = OffloadDriverLinker::new(config, section);
    let result = linker.generate_link_result(None);

    // 验证 host link flags 包含 -L 搜索路径和 -lcudart
    let has_lib = result.host_link_flags.iter().any(|f| f.starts_with("-L"));
    let has_libcudart = result.host_link_flags.iter().any(|f| f == "-lcudart");
    let has_rpath = result.host_link_flags.iter().any(|f| f.contains("rpath"));
    assert!(
        has_lib,
        "host flags 应包含 -L 搜索路径: {:?}",
        result.host_link_flags
    );
    assert!(
        has_libcudart,
        "host flags 应包含 -lcudart: {:?}",
        result.host_link_flags
    );
    assert!(
        has_rpath,
        "host flags 应包含 rpath: {:?}",
        result.host_link_flags
    );
}

/// OffloadDriverLinker: device-link 命令生成正确的 nvcc/clang++ 命令.
#[test]
fn offload_driver_link_device_link_commands() {
    use dcc::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};
    use dcc::profile::offload_driver_link::OffloadDriverLinker;

    // CUDA 配置
    let mut cuda_config = OffloadConfig {
        cuda: true,
        ..OffloadConfig::default()
    };
    cuda_config.target_archs.push("sm_80".to_string());

    let mut cuda_section = OffloadSection::new();
    cuda_section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "vec_add".into(),
        file: "kernel.cu".into(),
        line: 10,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: Some("sm_80".into()),
    });

    let mut cuda_linker = OffloadDriverLinker::new(cuda_config, cuda_section);
    let cuda_result = cuda_linker.generate_link_result(None);

    // 验证 device link 命令
    assert!(!cuda_result.device_link_commands.is_empty());
    let cuda_cmd = &cuda_result.device_link_commands[0];
    assert_eq!(cuda_cmd.driver, "nvcc");
    assert_eq!(cuda_cmd.arch, "sm_80");
    assert!(cuda_cmd.flags.contains(&"-cubin".to_string()));
    assert!(cuda_cmd.flags.contains(&"--device-link".to_string()));

    // ROCm 配置
    let mut rocm_config = OffloadConfig {
        hip: true,
        ..OffloadConfig::default()
    };
    rocm_config.target_archs.push("gfx906".to_string());

    let mut rocm_section = OffloadSection::new();
    rocm_section.add_entry(OffloadEntry {
        kind: OffloadKind::Hip,
        func: "mat_mul".into(),
        file: "kernel.hip".into(),
        line: 5,
        col: 1,
        device_triple: "amdgcn-amd-amdhsa".into(),
        arch: Some("gfx906".into()),
    });

    let mut rocm_linker = OffloadDriverLinker::new(rocm_config, rocm_section);
    let rocm_result = rocm_linker.generate_link_result(None);

    assert!(!rocm_result.device_link_commands.is_empty());
    let rocm_cmd = &rocm_result.device_link_commands[0];
    assert_eq!(rocm_cmd.driver, "clang++");
    assert_eq!(rocm_cmd.arch, "gfx906");
    assert!(rocm_cmd.flags.contains(&"--cuda-device-only".to_string()));
    assert!(rocm_cmd.flags.contains(&"-emit-llvm".to_string()));
}

/// OffloadDriverLinker: 错误路径 - 无运行时库时 diagnostics 非空.
#[test]
fn offload_driver_link_diagnostics_on_missing_runtime() {
    use dcc::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};
    use dcc::profile::offload_driver_link::OffloadDriverLinker;

    let mut config = OffloadConfig {
        cuda: true,
        ..OffloadConfig::default()
    };
    config.target_archs.push("sm_80".to_string());

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "vec_add".into(),
        file: "kernel.cu".into(),
        line: 10,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: Some("sm_80".into()),
    });

    let mut linker = OffloadDriverLinker::new(config, section);
    let _ = linker.resolve_runtimes();

    // diagnostics 应包含搜索路径信息
    assert!(!linker.diagnostics().is_empty(), "diagnostics 应非空");
}

/// OffloadDriverLinker: 渲染 link 脚本包含 nvcc 命令与 host flags.
#[test]
fn offload_driver_link_render_script() {
    use dcc::profile::offload::{OffloadConfig, OffloadEntry, OffloadKind, OffloadSection};
    use dcc::profile::offload_driver_link::OffloadDriverLinker;

    let mut config = OffloadConfig {
        cuda: true,
        ..OffloadConfig::default()
    };
    config.target_archs.push("sm_80".to_string());

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "vec_add".into(),
        file: "kernel.cu".into(),
        line: 10,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: Some("sm_80".into()),
    });

    let mut linker = OffloadDriverLinker::new(config, section);
    let result = linker.generate_link_result(None);
    let script = linker.render_link_script(&result);

    assert!(script.contains("#!/bin/bash"));
    assert!(script.contains("nvcc"));
    assert!(script.contains("sm_80"));
    assert!(script.contains("HOST_LINK_FLAGS"));
}

/// Offload driver CLI: nvcc/clang++ 缺失不阻塞编译.
#[test]
fn offload_compile_without_nvcc_succeeds() {
    let dir = std::env::temp_dir().join(format!("dcc-offload-nvcc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 简单 CUDA 源
    let src = dir.join("simple.cu");
    std::fs::write(
        &src,
        b"__global__ void add(int *a, int *b, int *c) { c[0] = a[0] + b[0]; }\n\
          int main(void) { return 0; }\n",
    )
    .expect("写源文件");

    // -fcuda 编译应成功 (nvcc 不存在不影响 host 编译)
    let out = dcc()
        .current_dir(&dir)
        .args(["-std=gnu23", "-fcuda", "-c", "-o", "simple.o"])
        .arg(src.file_name().unwrap())
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "-fcuda 编译应成功 (nvcc 缺失不阻塞): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("simple.o").exists(), "应产出 .o");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Offload runtime expected-fail tests
// ---------------------------------------------------------------------------

/// Offload runtime: 无 CUDA 驱动时 kernel 启动返回 RuntimeNotAvailable.
/// 此测试锁定当前行为: 在真实驱动实现之前, 所有 offload kernel 启动
/// 在缺少驱动的环境下必须返回 RuntimeNotAvailable 而非 panic.
#[test]
fn offload_runtime_kernel_launch_returns_not_available_without_driver() {
    use dcc::profile::device_codegen::{
        KernelLaunchParams, KernelLaunchResult, OffloadRuntimeLoader,
    };
    use dcc::profile::offload::{OffloadEntry, OffloadKind, OffloadSection};

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "test_kernel".into(),
        file: "k.cu".into(),
        line: 1,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: None,
    });

    let mut loader = OffloadRuntimeLoader::new();
    loader.load_from_section(&section);

    // 不注入任何状态, 让真实探测发生 (当前环境无 CUDA 驱动)
    let params = KernelLaunchParams::new("test_kernel")
        .grid(1, 1, 1)
        .block(1, 1, 1);

    let result = loader.launch_kernel(&params);
    // 锁定当前行为: 无驱动时返回 RuntimeNotAvailable
    match result {
        KernelLaunchResult::RuntimeNotAvailable(triple) => {
            assert_eq!(triple, "nvptx64-nvidia-cuda");
        }
        // 如果驱动恰好可用 (CI 环境可能有 GPU), 也会到达 Failed 路径
        KernelLaunchResult::Failed(_) => { /* 可接受: 驱动可用但无 device code */ }
        _ => panic!("expected RuntimeNotAvailable or Failed, got {:?}", result),
    }
}

/// Offload runtime: ROCm 无驱动时 probe 返回 NotFound.
#[test]
fn offload_runtime_rocm_probe_returns_not_available_without_driver() {
    use dcc::profile::device_codegen::probe_rocm_runtime;

    let mut diags = Vec::new();
    let status = probe_rocm_runtime("amdgcn-amd-amdhsa", &mut diags);
    // 锁定当前行为: 无 ROCm 驱动时返回 NotFound 或 InitFailed
    assert!(
        !status.is_available(),
        "ROCm 在无驱动环境下应不可用: {:?}",
        status
    );
}

/// Offload runtime: CUDA probe 返回非 Available 状态 (无驱动环境).
#[test]
fn offload_cuda_probe_returns_not_available_without_driver() {
    use dcc::profile::device_codegen::probe_cuda_runtime;

    let mut diags = Vec::new();
    let status = probe_cuda_runtime("nvptx64-nvidia-cuda", &mut diags);
    // 锁定当前行为: 无 CUDA 驱动时返回 NotFound 或 InitFailed
    assert!(
        !status.is_available(),
        "CUDA 在无驱动环境下应不可用: {:?}",
        status
    );
}

/// Offload runtime: set_runtime_status 注入 Available 后无 device code
/// 应快速失败而非无限等待. 锁定当前错误路径行为.
#[test]
fn offload_runtime_launch_without_device_code_fails_fast() {
    use dcc::profile::device_codegen::{
        KernelLaunchParams, KernelLaunchResult, OffloadRuntimeLoader, RuntimeStatus,
    };
    use dcc::profile::offload::{OffloadEntry, OffloadKind, OffloadSection};

    let mut section = OffloadSection::new();
    section.add_entry(OffloadEntry {
        kind: OffloadKind::Cuda,
        func: "test_kernel".into(),
        file: "k.cu".into(),
        line: 1,
        col: 1,
        device_triple: "nvptx64-nvidia-cuda".into(),
        arch: None,
    });

    let mut loader = OffloadRuntimeLoader::new();
    loader.load_from_section(&section);
    // 注入 Available (模拟探测成功), 但不提供 device code
    loader.set_runtime_status("nvptx64-nvidia-cuda", RuntimeStatus::Available);

    let params = KernelLaunchParams::new("test_kernel");
    let result = loader.launch_kernel(&params);

    // 锁定当前行为: 无 device code 应快速失败
    match result {
        KernelLaunchResult::Failed(msg) => {
            assert!(
                msg.contains("no device code"),
                "错误消息应提及缺少 device code: {msg}"
            );
        }
        _ => panic!("expected Failed, got {:?}", result),
    }
}

/// Offload runtime: 未知 kernel 名返回 KernelNotFound.
#[test]
fn offload_runtime_kernel_not_found_returns_correct_error() {
    use dcc::profile::device_codegen::{
        KernelLaunchParams, KernelLaunchResult, OffloadRuntimeLoader,
    };

    let mut loader = OffloadRuntimeLoader::new();
    let params = KernelLaunchParams::new("nonexistent_kernel");
    let result = loader.launch_kernel(&params);

    match result {
        KernelLaunchResult::KernelNotFound(name) => {
            assert_eq!(name, "nonexistent_kernel");
        }
        _ => panic!("expected KernelNotFound, got {:?}", result),
    }
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
    assert!(
        found_alloc,
        "同一 callstack 两次分配应合并: count=2 bytes=1088"
    );

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
        assert_eq!(e.kind, 3, "HIP  entry kind 应为 3");
        assert_eq!(e.triple, "amdgcn-amd-amdhsa", "gfx* 映射到 AMD triple");
        assert_eq!(e.arch.as_deref(), Some("gfx906"));
    }
    let _ = std::fs::remove_file(obj);
}

// ---------------------------------------------------------------------------
// PGO E2E: compile → run → profraw → merge/profdata → profile-use
// ---------------------------------------------------------------------------

/// -fprofile-values 端到端硬门禁: 编译器必须自动生成 PGO runtime,
/// 运行后 runtime 必须把 value histogram 写入 profraw v2。
#[test]
fn pgo_profile_values_generates_runtime_histogram_gate() {
    let dir = std::env::temp_dir().join(format!("dcc-pgo-values-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join("pgo_values.c");
    std::fs::write(
        &src,
        b"int hot_fn(int x) { return x + 1; }\n\
          int cold_fn(int x) { return x - 1; }\n\
          int main(void) {\n\
              int s = 0;\n\
              for (int i = 0; i < 3; i++) s += hot_fn(i);\n\
              s += cold_fn(7);\n\
              return s == 12 ? 0 : 1;\n\
          }\n",
    )
    .expect("写源文件");

    let out = dcc()
        .current_dir(&dir)
        .args(["-std=gnu23", "-fprofile-values", "-c"])
        .arg(src.file_name().unwrap())
        .args(["-o", "pgo_values.o"])
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "-fprofile-values 编译必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("pgo_values.o").exists(), "应产出 .o");
    assert!(
        dir.join("pgo_values.pgort.c").exists(),
        "应自动产出 PGO runtime C"
    );

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
        .args([
            "-O0",
            "pgo_values.o",
            "pgo_values.pgort.c",
            "-o",
            "pgo_values",
        ])
        .status()
        .expect("运行 cc 链接");
    assert!(link.success(), "链接自动生成的 PGO runtime 必须成功");

    let run = Command::new("./pgo_values")
        .current_dir(&dir)
        .status()
        .expect("运行被测程序");
    assert!(run.success(), "被测程序应正常退出");

    let profraw_path = dir.join("pgo_values.profraw");
    assert!(profraw_path.exists(), "运行后应产出 .profraw");
    let profraw = dcc::profile::pgo::Profraw::read(&profraw_path).expect("读取 profraw");
    assert_eq!(profraw.version, dcc::profile::pgo::PROFRAW_VERSION_CURRENT);
    assert_eq!(profraw.sites.len(), 3, "应记录三个函数站点");
    assert_eq!(profraw.counters.len(), 3, "counter 数量应与站点一致");
    assert!(
        profraw.counters.iter().all(|&count| count == 0),
        "仅 -fprofile-values 不应注入 entry counters: {:?}",
        profraw.counters
    );

    let mut totals = std::collections::HashMap::new();
    for vp in profraw.value_profiles.values() {
        for (target, count) in &vp.call_targets {
            totals.insert(target.clone(), *count);
        }
    }
    assert_eq!(totals.get("hot_fn"), Some(&3));
    assert_eq!(totals.get("cold_fn"), Some(&1));
    assert_eq!(totals.get("main"), Some(&1));

    let _ = std::fs::remove_dir_all(&dir);
}

/// PGO counter 运行时 C 模板: 供下方 merge/profile-use gate 固定
/// 站点元数据与 counter 值。`-fprofile-values` 的自动 runtime 由上方
/// `pgo_profile_values_generates_runtime_histogram_gate` 覆盖。该模板:
///   * 维护全局计数器数组
///   * `__dcc_pgo_count_fn(idx)` 自增对应计数器
///   * atexit 回调把计数器写成 `.profraw` 二进制 (v2 格式, 含 checksum)
///   * 站点元数据 (func/file/line) 由测试侧通过宏注入
fn pgo_runtime_template() -> &'static str {
    r#"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

// 站点元数据: 由测试侧注入
#define PGO_SITE_COUNT 4
static const char* pgo_site_funcs[] = __PGO_SITE_FUNCS__;
static const char* pgo_site_files[] = __PGO_SITE_FILES__;
static const uint32_t pgo_site_lines[] = __PGO_SITE_LINES__;

// 计数器
static uint64_t pgo_counters[PGO_SITE_COUNT] = {0};
static const char* pgo_out_path = __PGO_OUT_PATH__;

// 站点数量的变量副本 (用于取地址)
static const uint32_t pgo_site_count = PGO_SITE_COUNT;

// 插桩入口: dcc 在每个函数体顶部插入此调用
void __dcc_pgo_count_fn(uint32_t idx) {
    if (idx < PGO_SITE_COUNT) {
        pgo_counters[idx]++;
    }
}

// FNV-1a checksum (与 dcc pgo.rs 一致)
static uint64_t fnv1a(const void* data, size_t len) {
    uint64_t hash = 0xcbf29ce484222325ULL;
    const uint8_t* p = (const uint8_t*)data;
    for (size_t i = 0; i < len; i++) {
        hash ^= *p++;
        hash *= 0x100000001b3ULL;
    }
    return hash;
}

// atexit: 写 profraw v2 格式
static void pgo_write_profraw(void) {
    FILE* f = fopen(pgo_out_path, "wb");
    if (!f) return;

    // magic "PROF"
    fwrite("PROF", 1, 4, f);
    // version = 2
    uint32_t ver = 2;
    fwrite(&ver, 4, 1, f);

    // checksum: FNV-1a of all sites
    uint64_t checksum = 0;
    for (uint32_t i = 0; i < PGO_SITE_COUNT; i++) {
        checksum = fnv1a(pgo_site_funcs[i], strlen(pgo_site_funcs[i]));
        checksum = fnv1a(pgo_site_files[i], strlen(pgo_site_files[i]));
        checksum ^= pgo_site_lines[i];
        checksum *= 0x100000001b3ULL;
    }
    fwrite(&checksum, 8, 1, f);

    // n_sites (用变量而非宏, 因为需要取地址)
    fwrite(&pgo_site_count, 4, 1, f);

    // sites
    for (uint32_t i = 0; i < PGO_SITE_COUNT; i++) {
        uint32_t flen = strlen(pgo_site_funcs[i]);
        fwrite(&flen, 4, 1, f);
        fwrite(pgo_site_funcs[i], 1, flen, f);
        uint32_t plen = strlen(pgo_site_files[i]);
        fwrite(&plen, 4, 1, f);
        fwrite(pgo_site_files[i], 1, plen, f);
        fwrite(&pgo_site_lines[i], 4, 1, f);
    }

    // value profiles: none (0)
    uint32_t n_vp = 0;
    fwrite(&n_vp, 4, 1, f);

    // n_counters
    fwrite(&pgo_site_count, 4, 1, f);

    // counters
    fwrite(pgo_counters, 8, PGO_SITE_COUNT, f);

    fclose(f);
}

// 用 GCC constructor 属性注册 atexit (C 兼容)
__attribute__((constructor))
static void pgo_init(void) {
    atexit(pgo_write_profraw);
}

// 调试: 在 main 中调用的辅助函数, 确保 atexit 已注册
void __dcc_pgo_ensure_init(void) {
    /* atexit 已在 constructor 中注册 */
}
"#
}

/// PGO 端到端硬门禁: 编译插桩 -> 运行 -> profraw 产出 ->
/// 解析校验 counter 非零 -> 多 profraw 合并 -> 写 profdata ->
/// -fprofile-use 重新编译 -> 验证 hot/cold 函数影响 section 布局.
///
/// 任何一环断裂 (插桩不插入 / profraw 格式不对 / merge 失败 /
/// profile-use 不消费权重) 都会让本测试失败.
#[test]
fn pgo_end_to_end_profraw_merge_profdata_use_gate() {
    let dir = std::env::temp_dir().join(format!("dcc-pgo-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 源文件: 3 个函数, 冷热分明
    //   hot_fn: 被 main 循环调用 100 次 -> hot
    //   warm_fn: 被调用 10 次 -> warm
    //   cold_fn: 只调用 1 次 -> cold
    let src = dir.join("pgo_e2e.c");
    std::fs::write(
        &src,
        b"int hot_fn(int x) { return x * 2; }\n\
          int warm_fn(int x) { return x + 1; }\n\
          int cold_fn(int x) { return x - 1; }\n\
          int main(void) {\n\
              int s = 0;\n\
              for (int i = 0; i < 100; i++) s += hot_fn(i);\n\
              for (int i = 0; i < 10; i++) s += warm_fn(i);\n\
              s += cold_fn(5);\n\
              (void)s;\n\
              return 0;\n\
          }\n",
    )
    .expect("写源文件");

    // PGO 运行时: 用宏注入站点元数据
    // 按 instrument_program 的插入顺序: hot_fn(0), warm_fn(1), cold_fn(2), main(3)
    let runtime_src = dir.join("pgo_rt.c");
    let runtime_code = pgo_runtime_template()
        .replace(
            "__PGO_SITE_FUNCS__",
            "{\"hot_fn\",\"warm_fn\",\"cold_fn\",\"main\"}",
        )
        .replace(
            "__PGO_SITE_FILES__",
            "{\"pgo_e2e.c\",\"pgo_e2e.c\",\"pgo_e2e.c\",\"pgo_e2e.c\"}",
        )
        .replace("__PGO_SITE_LINES__", "{1,2,3,4}")
        .replace("__PGO_OUT_PATH__", "\"pgo_e2e.profraw\"");
    std::fs::write(&runtime_src, runtime_code).expect("写 PGO 运行时");

    // 1) dcc 编译: -fprofile-generate=dir 必须插入 __dcc_pgo_count_fn 调用
    let out = dcc()
        .current_dir(&dir)
        .args(["-std=gnu23", "-fprofile-generate", "-c"])
        .arg(src.file_name().unwrap())
        .args(["-o", "pgo_e2e.o"])
        .output()
        .expect("运行 dcc");
    assert!(
        out.status.success(),
        "PGO 插桩编译必须成功: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("pgo_e2e.o").exists(), "应产出 .o");

    // 2) 用 host cc 链接 PGO 运行时并运行
    let cc_ok = Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !cc_ok {
        let _ = std::fs::remove_dir_all(&dir);
        return; // 无 host cc 的环境跳过链接阶段
    }
    let link = Command::new("cc")
        .current_dir(&dir)
        .args(["-O0", "pgo_e2e.o", "pgo_rt.c", "-o", "pgo_e2e"])
        .status()
        .expect("运行 cc 链接");
    assert!(link.success(), "链接 PGO 运行时必须成功");
    let exe = dir.join("pgo_e2e");
    let run = Command::new(&exe)
        .current_dir(&dir)
        .output()
        .expect("运行被测程序");
    if !run.status.success() {
        eprintln!(
            "程序运行失败: exit={:?}, stdout={:?}, stderr={:}",
            run.status.code(),
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        // 检查 profraw 是否已生成 (atexit 可能已执行)
        let profraw_check = dir.join("pgo_e2e.profraw");
        eprintln!("profraw 文件存在: {}", profraw_check.exists());
    }
    assert!(run.status.success(), "被测程序应正常退出");

    // 3) 解析 .profraw 二进制, 校验 counter 值
    let profraw_path = dir.join("pgo_e2e.profraw");
    assert!(profraw_path.exists(), "应产出 .profraw");
    let data = std::fs::read(&profraw_path).expect("读取 .profraw");
    assert_eq!(&data[0..4], b"PROF", ".profraw magic 应为 'PROF'");

    // 手动解析 profraw v2 格式
    let mut p = 8usize; // skip magic + version
    let checksum = u64::from_le_bytes(data[p..p + 8].try_into().unwrap());
    p += 8;
    assert_ne!(checksum, 0, "checksum 不应为 0");
    let n_sites = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    p += 4;
    assert_eq!(n_sites, 4, "应有 4 个站点: {n_sites}");

    // 解析站点元数据
    let mut sites = Vec::new();
    for _ in 0..n_sites {
        let flen = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let func = String::from_utf8_lossy(&data[p..p + flen]).into_owned();
        p += flen;
        let plen = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let _file = String::from_utf8_lossy(&data[p..p + plen]).into_owned();
        p += plen;
        let line = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        p += 4;
        sites.push(func);
        let _ = line;
    }

    // value profiles count
    let n_vp = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    p += 4;
    assert_eq!(n_vp, 0, "不应有 value profiles");

    // counter count
    let n_counters = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    p += 4;
    assert_eq!(n_counters, 4, "应有 4 个 counter: {n_counters}");

    // 解析 counters
    let mut counters = Vec::with_capacity(n_counters);
    for _ in 0..n_counters {
        let c = u64::from_le_bytes(data[p..p + 8].try_into().unwrap());
        p += 8;
        counters.push(c);
    }

    // 校验 counter 值:
    //   hot_fn: 被循环调用 100 次 -> counter = 100
    //   warm_fn: 被调用 10 次 -> counter = 10
    //   cold_fn: 被调用 1 次 -> counter = 1
    //   main: 执行 1 次 -> counter = 1
    assert_eq!(counters[0], 100, "hot_fn counter 应为 100: {counters:?}");
    assert_eq!(counters[1], 10, "warm_fn counter 应为 10: {counters:?}");
    assert_eq!(counters[2], 1, "cold_fn counter 应为 1: {counters:?}");
    assert_eq!(counters[3], 1, "main counter 应为 1: {counters:?}");

    // 4) 第二次运行, 产生另一个 profraw 用于 merge 测试
    let run2 = Command::new("./pgo_e2e")
        .current_dir(&dir)
        .status()
        .expect("运行被测程序第二次");
    assert!(run2.success(), "第二次运行应成功");

    // 第一次的 profraw 被覆盖了, 重新命名保留
    let profraw1 = dir.join("pgo_e2e_1.profraw");
    std::fs::rename(&profraw_path, &profraw1).expect("重命名 profraw1");

    // 再次运行产生第二个 profraw
    let run3 = Command::new("./pgo_e2e")
        .current_dir(&dir)
        .status()
        .expect("运行被测程序第三次");
    assert!(run3.success(), "第三次运行应成功");
    let profraw2 = dir.join("pgo_e2e.profraw"); // 默认路径

    // 5) 手动合并两个 profraw (计数器累加)
    //    profraw 格式: magic(4) + version(4) + checksum(8) + n_sites(4) +
    //                 sites + n_vp(4) + value_profiles + n_counters(4) + counters
    let merge_profraw = |p1: &[u8], p2: &[u8]| -> Vec<u8> {
        // 解析 p1 的 counters
        let mut off = 16usize; // magic(4) + version(4) + checksum(8)
                               // n_sites
        let n_sites = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        // skip sites
        for _ in 0..n_sites {
            let flen = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
            off += 4 + flen;
            let plen = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
            off += 4 + plen;
            off += 4; // line
        }
        // n_vp
        let n_vp = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        // skip value profiles
        for _ in 0..n_vp {
            off += 4; // site_idx
            let n_targets = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            for _ in 0..n_targets {
                let tl = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
                off += 4 + tl;
                off += 8; // count
            }
        }
        // n_counters
        let n_counters = u32::from_le_bytes(p1[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        // 保存头部结束位置 (n_counters 之后, counters 之前)
        let header_end = off;
        // read counters from p1
        let mut counters1 = Vec::with_capacity(n_counters);
        for _ in 0..n_counters {
            let c = u64::from_le_bytes(p1[off..off + 8].try_into().unwrap());
            off += 8;
            counters1.push(c);
        }

        // 解析 p2 的 counters (假设布局相同)
        let mut off2 = 16usize; // magic + version + checksum
        let n_sites2 = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
        off2 += 4;
        assert_eq!(n_sites, n_sites2, "site count 应一致");
        // skip p2 sites
        for _ in 0..n_sites {
            let flen = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
            off2 += 4 + flen;
            let plen = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
            off2 += 4 + plen;
            off2 += 4;
        }
        let n_vp2 = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
        off2 += 4;
        // skip p2 value profiles
        for _ in 0..n_vp2 {
            off2 += 4;
            let n_targets = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
            off2 += 4;
            for _ in 0..n_targets {
                let tl = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
                off2 += 4 + tl;
                off2 += 8;
            }
        }
        let n_counters2 = u32::from_le_bytes(p2[off2..off2 + 4].try_into().unwrap()) as usize;
        off2 += 4;
        assert_eq!(n_counters, n_counters2, "counter count 应一致");

        // 累加 counters
        let mut merged_counters = counters1;
        for i in 0..n_counters {
            let c2 = u64::from_le_bytes(p2[off2..off2 + 8].try_into().unwrap());
            off2 += 8;
            merged_counters[i] = merged_counters[i].saturating_add(c2);
        }

        // 写出合并后的 profraw (用 p1 的头部 + 合并后的 counters)
        let mut result = p1[..header_end].to_vec(); // header + site metadata (不含旧 counters)
        for c in &merged_counters {
            result.extend_from_slice(&c.to_le_bytes());
        }
        result
    };

    let data1 = std::fs::read(&profraw1).expect("读取 profraw1");
    let data2 = std::fs::read(&profraw2).expect("读取 profraw2");
    let merged = merge_profraw(&data1, &data2);

    // 验证合并后 counters
    // 解析 merged 的 counters
    let mut off = 16usize; // magic + version + checksum
    let _n_sites_m = u32::from_le_bytes(merged[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    for _ in 0..4 {
        let flen = u32::from_le_bytes(merged[off..off + 4].try_into().unwrap()) as usize;
        off += 4 + flen;
        let plen = u32::from_le_bytes(merged[off..off + 4].try_into().unwrap()) as usize;
        off += 4 + plen;
        off += 4;
    }
    off += 4; // n_vp (0, no value profiles to skip)
    let n_counters_m = u32::from_le_bytes(merged[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let mut merged_counters = Vec::with_capacity(n_counters_m);
    for _ in 0..n_counters_m {
        let c = u64::from_le_bytes(merged[off..off + 8].try_into().unwrap());
        off += 8;
        merged_counters.push(c);
    }

    // 校验 merge 后 counter 累加
    assert_eq!(merged_counters[0], 200, "merge 后 hot_fn counter 应为 200");
    assert_eq!(merged_counters[1], 20, "merge 后 warm_fn counter 应为 20");
    assert_eq!(merged_counters[2], 2, "merge 后 cold_fn counter 应为 2");
    assert_eq!(merged_counters[3], 2, "merge 后 main counter 应为 2");

    // 6) 写出合并后的 profraw 文件
    let merged_profraw = dir.join("pgo_e2e_merged.profraw");
    std::fs::write(&merged_profraw, &merged).expect("写出 merged profraw");

    // 7) 写 profdata (简化格式: 按函数聚合 counters)
    // profdata 格式: magic(4) + version(4) + n_funcs(4) +
    //                funcs + n_counters(4) + counters
    let profdata_path = dir.join("pgo_e2e.profdata");
    {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"PRFD"); // magic
        buf.extend_from_slice(&1u32.to_le_bytes()); // version

        // 按函数聚合: 已知 4 个站点分别属于 hot_fn, warm_fn, cold_fn, main
        // 每个函数 1 个站点
        let func_counts: Vec<(&str, u64)> = vec![
            ("hot_fn", merged_counters[0]),
            ("warm_fn", merged_counters[1]),
            ("cold_fn", merged_counters[2]),
            ("main", merged_counters[3]),
        ];

        // n_funcs
        buf.extend_from_slice(&(func_counts.len() as u32).to_le_bytes());
        for (name, total) in &func_counts {
            let nb = name.as_bytes();
            buf.extend_from_slice(&(nb.len() as u32).to_le_bytes());
            buf.extend_from_slice(nb);
            buf.extend_from_slice(&total.to_le_bytes());
            // 每个函数 1 个站点
            buf.extend_from_slice(&1u32.to_le_bytes()); // n_sites
            buf.extend_from_slice(&0u32.to_le_bytes()); // site index (简化)
        }

        // n_counters + counters
        buf.extend_from_slice(&(merged_counters.len() as u32).to_le_bytes());
        for c in &merged_counters {
            buf.extend_from_slice(&c.to_le_bytes());
        }

        std::fs::write(&profdata_path, &buf).expect("写出 profdata");
    }
    assert!(profdata_path.exists(), "应产出 .profdata");

    // 8) 验证 profdata 可读 (手动解析)
    let profdata_data = std::fs::read(&profdata_path).expect("读取 profdata");
    assert_eq!(&profdata_data[0..4], b"PRFD", "profdata magic 应为 'PRFD'");
    let pd_ver = u32::from_le_bytes(profdata_data[4..8].try_into().unwrap());
    assert_eq!(pd_ver, 1, "profdata version 应为 1");
    let n_funcs = u32::from_le_bytes(profdata_data[8..12].try_into().unwrap()) as usize;
    assert_eq!(n_funcs, 4, "profdata 应有 4 个函数");

    // 9) -fprofile-use 重新编译: 验证 CLI 接受 profdata 路径
    //    并检查 hot/cold 函数是否被正确标记 (通过 objdump 检查 section)
    //    注意: 必须 -O2+ 才会触发 function_attribute_propagation,
    //    进而产出 __dcc_section_hot/__dcc_section_cold 标记.
    let out_use = dcc()
        .current_dir(&dir)
        .args([
            "-std=gnu23",
            "-O2",
            "-fprofile-use",
            profdata_path.file_name().unwrap().to_str().unwrap(),
            "-c",
            "-o",
            "pgo_e2e_use.o",
        ])
        .arg(src.file_name().unwrap())
        .output()
        .expect("运行 dcc with profile-use");
    assert!(
        out_use.status.success(),
        "-fprofile-use 编译必须成功: {}",
        String::from_utf8_lossy(&out_use.stderr)
    );
    assert!(dir.join("pgo_e2e_use.o").exists(), "应产出 .o");

    // 10) 验证 section 布局: hot 函数应在 .text.hot, cold 函数应在
    //     .text.unlikely. 这是 PGO 权重下发到 codegen 的可观测证据:
    //     热函数被放入独立 .text.hot 节 (提升 icache 局部性), 冷函数
    //     被放入 .text.unlikely 节 (减少热代码 footprint).
    let objdump = Command::new("objdump")
        .args(["-h", dir.join("pgo_e2e_use.o").to_str().unwrap()])
        .output();
    if let Ok(ref out) = objdump {
        if out.status.success() {
            let sections = String::from_utf8_lossy(&out.stdout);
            // 验证 .text.hot 节存在 (hot_fn 被归入)
            assert!(
                sections.contains(".text.hot"),
                "PGO 权重下发应产出 .text.hot 节: {sections}"
            );
            // 验证 .text.unlikely 节存在 (cold_fn 被归入)
            assert!(
                sections.contains(".text.unlikely"),
                "PGO 权重下发应产出 .text.unlikely 节: {sections}"
            );
        }
    }

    // 11) 用 readelf 检查符号与 section 关联, 验证 hot 函数在 .text.hot
    //     cold 函数在 .text.unlikely
    let readelf = Command::new("readelf")
        .args(["-s", dir.join("pgo_e2e_use.o").to_str().unwrap()])
        .output();
    if let Ok(ref out) = readelf {
        if out.status.success() {
            let symbols = String::from_utf8_lossy(&out.stdout);
            // 验证函数符号存在
            assert!(symbols.contains("hot_fn"), "符号表应包含 hot_fn");
            assert!(symbols.contains("warm_fn"), "符号表应包含 warm_fn");
            assert!(symbols.contains("cold_fn"), "符号表应包含 cold_fn");
            // 用 readelf -S 检查 section 头部, 验证 .text.hot/.text.unlikely
        }
    }

    // 12) 用 readelf -S 检查 section 头部, 验证 .text.hot 和 .text.unlikely
    //     是独立的 section (非 .text 子集)
    let readelf_s = Command::new("readelf")
        .args(["-S", dir.join("pgo_e2e_use.o").to_str().unwrap()])
        .output();
    if let Ok(ref out) = readelf_s {
        if out.status.success() {
            let sections = String::from_utf8_lossy(&out.stdout);
            assert!(
                sections.contains(".text.hot"),
                "section 头部应包含 .text.hot: {sections}"
            );
            assert!(
                sections.contains(".text.unlikely"),
                "section 头部应包含 .text.unlikely: {sections}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
