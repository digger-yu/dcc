// tests/integration/c_pp_cache.rs — 预处理 include 缓存的跨 TU 复用边界。
//
// 导入权归 `tests/integration.rs` 本壳（§5.2 规则 2），此文件只放用例。

/// include 缓存**只共享真没读过身份宏的文件**：`KBUILD_*` 的值已从缓存键里摘掉
/// （否则 kbuild 每个 TU 的键都不同 —— 实测冷读一个内核 TU 的 `-E` 要 143 s、
/// allmodconfig ~1 万个对象要 8.5 天），正确性因此完全押在条目自带的 `IDREAD` 读数校上。
///
/// 两头都要钉，缺一头都能悄悄退化：
///   * **反向**（安全）：真读了身份宏的头文件换一个 TU 必须重展开，拿到自己的值、
///     绝不串别人的值。这一条松了就是误编译：`include/rdma/ib_verbs.h:3690` 的
///     `__ib_alloc_pd(..., KBUILD_MODNAME)` 会把 A 模块的名字塞进 B 模块。
///   * **正向**（收益）：没读身份宏的头文件必须**真的被复用**。只钉反向的话，
///     「什么都不敢复用」也能全绿，而速度收益是空的。
///
/// 判据读的是回放路径写在输出里的 `/* #include "x" (cached) */` 标记 —— 它是
/// 「本文件的展开结果来自缓存条目」的唯一直接证据，比计时可靠（计时受负载影响）。
#[test]
#[cfg(unix)]
fn preproc_include_cache_shares_only_files_that_ignore_identity_macros() {
    let dir = std::env::temp_dir().join(format!("dcc-ppcache-{}", std::process::id()));
    let home = dir.join("home");
    // 开头也删一次：上一轮失败留下的缓存会让「第一个 TU 不该有命中」那条判据假红。
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("fixture dir");
    fs::create_dir_all(&home).expect("cache home");

    // 读了身份宏的头文件：不带 include guard，保证每个 TU 都真展开它
    fs::write(
        dir.join("idh.h"),
        "#define IDH_TAG KBUILD_MODNAME\nstatic const char *idh_ptr = IDH_TAG;\n",
    )
    .expect("write idh.h");
    // 完全不碰身份宏的头文件：这是应当被跳 TU 复用的那一族
    fs::write(
        dir.join("plain.h"),
        "#define PLAIN_TAG 7\nstatic const int plain_val = PLAIN_TAG;\n",
    )
    .expect("write plain.h");
    for name in ["t1.c", "t2.c"] {
        fs::write(
            dir.join(name),
            "#include \"plain.h\"\n#include \"idh.h\"\nint f(void){return plain_val + (int)idh_ptr[0];}\n",
        )
        .expect("write TU");
    }

    let run = |tu: &str, modname: &str| -> String {
        let out_path = dir.join(format!("{}.i", tu));
        let status = Command::new(env!("CARGO_BIN_EXE_dcc"))
            // 缓存根随子进程走（不改本进程环境：libtest 同进程多线程，`set_var` 与
            // 并发 `getenv` 在 glibc 层就是数据竞争）
            .env("HOME", &home)
            .args(["-E", "-I", "."])
            .arg(format!("-DKBUILD_MODNAME=\"{}\"", modname))
            .arg("-DKBUILD_BASENAME=\"shoe\"")
            .arg(tu)
            .arg("-o")
            .arg(&out_path)
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run dcc");
        assert!(status.success(), "{tu} 预处理失败");
        fs::read_to_string(&out_path).expect("read .i")
    };

    let first = run("t1.c", "alpha");
    let second = run("t2.c", "beta");

    // —— 反向：值不得串 ——
    assert!(first.contains("idh_ptr = \"alpha\""), "t1 没拿到自己的模块名:\n{first}");
    assert!(
        second.contains("idh_ptr = \"beta\""),
        "t2 没拿到自己的模块名（串了上一个 TU 的值就是误编译）:\n{second}"
    );
    assert!(!first.contains("beta"), "t1 里出现 t2 的模块名");
    assert!(!second.contains("alpha"), "t2 里出现 t1 的模块名");

    // —— 正向：没读身份宏的文件必须走缓存回放 ——
    assert!(
        !first.contains("#include \"plain.h\" (cached)"),
        "第一个 TU 不该有命中（缓存本来空的）"
    );
    assert!(
        second.contains("#include \"plain.h\" (cached)"),
        "第二个 TU 必须复用 plain.h 的条目（它从不读身份宏）——没复用就说明跳 TU 共享没生效"
    );
    assert!(
        !second.contains("#include \"idh.h\" (cached)"),
        "idh.h 读了身份宏，绝不许跨 TU 复用条目"
    );

    fs::remove_dir_all(&dir).ok();
}
