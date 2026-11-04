// src/link/host_ld.rs - 宿主 GNU ld 直驱（链接步骤不起 gcc/cc）
//
// 背景：`dcc a.c -o prog` 的历史实现是「先编临时 .o，再调 host cc 链接」。
// 那样链接一步仍会起 gcc，于是内核构建里 HOSTCC 走到链接时无法满足「全链不许出现
// gcc」，`scripts/dcc-host-wrapper.sh` 只好在 bash 里自己拼 crt1/crti/crtn + `-lc`
// + `-dynamic-linker` —— 同一套链接语义被迫存在两份（bash 一份、cc 一份）。
//
// 本模块把那一套搬进 dcc 自己：发现宿主 crt 启动文件、libc 搜索路径与动态解释器，
// 用 `ld` 直接链出可执行文件。cc 只在**环境不可用**时兜底（缺 ld/crt/解释器、
// 交叉目标、`-static` 等未建模形状），且必须由调用方显式说出走了哪条路。
//
// 发现顺序与 gcc 驱动逐条对齐（任一环缺失就报 Unavailable，不猜）：
//   crt1.o / crti.o / crtn.o     - /usr/lib/<multiarch>、/usr/lib64、/usr/lib …
//   crtbegin.o / crtend.o        - GCC 工具链目录（提供 __dso_handle/init_array 支撑）
//   libgcc.a                     - 同一目录（`_Float128`、128 位整型等会调 __fixtfsi 一族）
//   -lc                          - 交给 ld 自己的 SEARCH_DIR（GNU ld 内置了 multiarch 路径）
//   动态解释器                    - /lib64/ld-linux-x86-64.so.2 或 multiarch 下的同名文件

use std::path::{Path, PathBuf};
use std::process::Command;

/// 一次链接请求：与「用谁来链」无关的那部分，host cc / host ld 两条路共用。
pub struct LinkRequest<'a> {
    pub objects: &'a [PathBuf],
    pub output: &'a Path,
    pub link_libs: &'a [String],
    pub whole_archive_libs: &'a [String],
    pub search_paths: &'a [PathBuf],
    pub host_link_args: &'a [String],
    /// `-Wl,--dynamic-linker=` 显式覆盖；None 用本机默认解释器
    pub dynamic_linker: Option<&'a str>,
    pub sysroot: Option<&'a Path>,
    pub verbose: bool,
}

pub enum Outcome {
    Linked,
    /// 宿主环境不满足直驱条件 —— 调用方退回 host cc，并把原因讲出来
    Unavailable(String),
}

/// 探索到的宿主链接工具链。字段都是**已存在**的绝对路径（解释器除外，它是
/// 写进 PT_INTERP 的运行期路径，未必在当前 rootfs 可见）。
struct Toolchain {
    ld: String,
    crt_dir: PathBuf,
    crt1: PathBuf,
    crti: PathBuf,
    crtn: PathBuf,
    crtbegin: Option<PathBuf>,
    crtend: Option<PathBuf>,
    /// libgcc.a 所在目录；cc 驱动会自动把 `-lgcc` 加两次，直驱必须自己补
    libgcc_dir: Option<PathBuf>,
    interp: Option<String>,
    /// 传给 ld 的 --sysroot（仅当 crt 文件来自 sysroot 时）
    ld_sysroot: Option<PathBuf>,
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// 关掉直驱的唯一开关：`DCC_LINK_VIA=cc`（排障/对照用）。默认 `ld`。
fn driver_forced_away() -> Option<String> {
    match env_nonempty("DCC_LINK_VIA").as_deref() {
        Some("cc") => Some("DCC_LINK_VIA=cc".into()),
        _ => None,
    }
}

fn has_crt_triplet(dir: &Path) -> bool {
    dir.join("crt1.o").is_file() && dir.join("crti.o").is_file() && dir.join("crtn.o").is_file()
}

/// crt1/crti/crtn 所在目录。`DCC_HOST_CRT_DIR` 显式覆盖；否则按多架构布局找。
///
/// sysroot 存在时**只**在 sysroot 内找：拿宿主 crt 去链 sysroot 的 libc 是最坏的
/// 混合，宁可退回 cc 让它按自己的规则处理。
fn find_crt_dir(sysroot: Option<&Path>) -> Option<(PathBuf, Option<PathBuf>)> {
    if let Some(sr) = sysroot {
        let rels = [
            "usr/lib/x86_64-linux-gnu",
            "lib/x86_64-linux-gnu",
            "usr/lib64",
            "lib64",
            "usr/lib",
            "lib",
        ];
        for rel in rels {
            let d = sr.join(rel);
            if has_crt_triplet(&d) {
                return Some((d, Some(sr.to_path_buf())));
            }
        }
        return None;
    }
    if let Some(d) = env_nonempty("DCC_HOST_CRT_DIR") {
        let d = PathBuf::from(d);
        if has_crt_triplet(&d) {
            return Some((d, None));
        }
    }
    for d in [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib/x86_64-linux-gnu",
        "/lib64",
        "/lib",
    ] {
        let d = PathBuf::from(d);
        if has_crt_triplet(&d) {
            return Some((d, None));
        }
    }
    None
}

/// GCC 工具链目录里的 crtbegin.o/crtend.o。
///
/// 为什么必须找它们：glibc 的 `atexit()` 在 `libc_nonshared.a` 里引用 `__dso_handle`，
/// 该符号由 crtbegin.o 提供；gcc 驱动自动加，直接 `ld` 必须自己加。实测缺它时
/// `scripts/sign-file`/`objtool` 报 `undefined reference to __dso_handle`。
/// 找不到**不判失败**（静态桩场景仍可链），只是少了 ABI 支撑对象。
fn find_crt_begin_end(crt_dir: &Path, sysroot: Option<&Path>) -> (Option<PathBuf>, Option<PathBuf>) {
    let ok = |d: &Path| {
        let b = d.join("crtbegin.o");
        let e = d.join("crtend.o");
        (b.is_file() && e.is_file()).then(|| (b, e))
    };
    if let Some(d) = env_nonempty("DCC_HOST_CRT_BEGIN_DIR") {
        if let Some(pair) = ok(Path::new(&d)) {
            return (Some(pair.0), Some(pair.1));
        }
    }
    if let Some(sr) = sysroot {
        if let Some(pair) = ok(sr) {
            return (Some(pair.0), Some(pair.1));
        }
    }
    if let Some(pair) = ok(crt_dir) {
        return (Some(pair.0), Some(pair.1));
    }
    // /usr/lib/gcc/<target>/*/crtbegin.o：版本号取**最大**的一个（目录名降序）。
    for base in ["/usr/lib/gcc/x86_64-linux-gnu", "/usr/lib/gcc"] {
        let base = Path::new(base);
        let mut versions: Vec<PathBuf> = match std::fs::read_dir(base) {
            Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(),
            Err(_) => continue,
        };
        versions.sort();
        for v in versions.iter().rev() {
            if let Some(pair) = ok(v) {
                return (Some(pair.0), Some(pair.1));
            }
            // /usr/lib/gcc/*/<target>/* ：多一层 target 目录
            let mut subs: Vec<PathBuf> = match std::fs::read_dir(v) {
                Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(),
                Err(_) => continue,
            };
            subs.sort();
            for s in subs.iter().rev() {
                if let Some(pair) = ok(s) {
                    return (Some(pair.0), Some(pair.1));
                }
            }
        }
    }
    (None, None)
}

/// 运行期动态解释器路径。显式覆盖优先；否则从两个常见位置里挑实际存在的那个，
/// 返回的字符串是**写进 PT_INTERP 的路径**（不是宿主绝对路径的 sysroot 展开）。
///
/// 覆盖入口：`-Wl,--dynamic-linker=…`（`explicit`）、`DCC_HOST_DYN_LINKER`、
/// `DCC_HOST_LD_SO`（后者是 wrapper 时代沿用的名字，保留以免旧脚本失效）。
fn find_interp(explicit: Option<&str>, sysroot: Option<&Path>) -> Option<String> {
    if let Some(p) = explicit {
        return Some(p.to_string());
    }
    if let Some(p) = env_nonempty("DCC_HOST_DYN_LINKER").or_else(|| env_nonempty("DCC_HOST_LD_SO")) {
        return Some(p);
    }
    for cand in ["/lib64/ld-linux-x86-64.so.2", "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"] {
        let visible = match sysroot {
            Some(sr) => sr.join(cand.trim_start_matches('/')).is_file(),
            None => Path::new(cand).is_file(),
        };
        if visible {
            return Some(cand.to_string());
        }
    }
    None
}

/// `libgcc.a` 所在目录。
///
/// 为什么必须有：cc 驱动隐式链 `-lgcc`，而 dcc 的 `_Float128`/`__int128` 运行期
/// 支撑（`__fixtfsi`、`__divti3` 一族）不在 libc 里。缺它时链接报
/// `undefined reference to __fixtfsi` —— 不是「优化不够」，是 ABI 支撑缺失。
/// 探测与 crtbegin 同一套布局，优先 `DCC_HOST_LIBGCC_DIR` 覆盖。
fn find_libgcc_dir() -> Option<PathBuf> {
    if let Some(d) = env_nonempty("DCC_HOST_LIBGCC_DIR") {
        let d = PathBuf::from(d);
        if d.join("libgcc.a").is_file() {
            return Some(d);
        }
    }
    for base in ["/usr/lib/gcc/x86_64-linux-gnu", "/usr/lib/gcc/x86_64-pc-linux-gnu", "/usr/lib/gcc"] {
        let base = Path::new(base);
        let mut versions: Vec<PathBuf> = match std::fs::read_dir(base) {
            Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(),
            Err(_) => continue,
        };
        versions.sort();
        for v in versions.iter().rev() {
            if v.join("libgcc.a").is_file() {
                return Some(v.clone());
            }
            let mut subs: Vec<PathBuf> = match std::fs::read_dir(v) {
                Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(),
                Err(_) => continue,
            };
            subs.sort();
            for s in subs.iter().rev() {
                if s.join("libgcc.a").is_file() {
                    return Some(s.clone());
                }
            }
        }
    }
    None
}

fn probe(req: &LinkRequest<'_>) -> Result<Toolchain, String> {
    let (crt_dir, ld_sysroot) = find_crt_dir(req.sysroot)
        .ok_or_else(|| "找不到 crt1.o/crti.o/crtn.o（可用 DCC_HOST_CRT_DIR 指定）".to_string())?;
    let (crtbegin, crtend) = find_crt_begin_end(&crt_dir, req.sysroot);
    let libgcc_dir = find_libgcc_dir();
    let interp = find_interp(req.dynamic_linker, req.sysroot);
    let ld = env_nonempty("DCC_LD").unwrap_or_else(|| "ld".to_string());
    Ok(Toolchain {
        ld,
        crt1: crt_dir.join("crt1.o"),
        crti: crt_dir.join("crti.o"),
        crtn: crt_dir.join("crtn.o"),
        crt_dir,
        crtbegin,
        crtend,
        libgcc_dir,
        interp,
        ld_sysroot,
    })
}

/// `-Wl,a,b,c` → `a b c`。cc 驱动会把它们原样转给 ld，直驱时必须自己拆。
fn translate_wl(arg: &str) -> Vec<String> {
    match arg.strip_prefix("-Wl,") {
        Some(rest) => rest.split(',').map(str::to_string).collect(),
        None => vec![arg.to_string()],
    }
}

/// 尝试用宿主 `ld` 直接链接。
///
/// 返回 `Ok(Unavailable)` 表示「这条路不适用」，调用方应退回 host cc 并如实告知；
/// `Err` 表示确实尝试过且链接失败 —— 不许静默改道（那正是本轮要消灭的形状）。
pub fn link(req: &LinkRequest<'_>) -> Result<Outcome, String> {
    if let Some(why) = driver_forced_away() {
        return Ok(Outcome::Unavailable(why));
    }
    if req.objects.is_empty() {
        return Ok(Outcome::Unavailable("no objects".into()));
    }
    if let Some(a) = req.host_link_args.iter().find(|a| a.as_str() == "-Wl,-static" || a.as_str() == "-static") {
        return Ok(Outcome::Unavailable(format!("{a}（静态链接未建模）")));
    }
    let tc = match probe(req) {
        Ok(tc) => tc,
        Err(why) => return Ok(Outcome::Unavailable(why)),
    };
    let Some(interp) = tc.interp.clone() else {
        return Ok(Outcome::Unavailable(
            "找不到动态解释器（可用 DCC_HOST_DYN_LINKER 指定）".into(),
        ));
    };

    let mut cmd = Command::new(&tc.ld);
    cmd.arg("-o").arg(req.output);
    cmd.arg("--eh-frame-hdr");
    cmd.arg("-m").arg("elf_x86_64");
    cmd.arg("-no-pie");
    cmd.arg("-z").arg("norelro");
    cmd.arg("-dynamic-linker").arg(&interp);
    if let Some(sr) = &tc.ld_sysroot {
        cmd.arg(format!("--sysroot={}", sr.display()));
    }
    cmd.arg(&tc.crt1).arg(&tc.crti);
    if let Some(b) = &tc.crtbegin {
        cmd.arg(b);
    }
    for o in req.objects {
        cmd.arg(o);
    }
    // `-Wl,` 透传参数（`-E`、`-rpath`、`-z now` 等）按 cc 的规则拆开交给 ld。
    // 显式 dynamic-linker 已经在上面统一处理，这里跳过重复项。
    for a in req.host_link_args {
        let parts = translate_wl(a);
        if parts.iter().any(|p| p == "-dynamic-linker" || p.starts_with("--dynamic-linker=")) {
            continue;
        }
        for p in parts {
            cmd.arg(p);
        }
    }
    for p in req.search_paths {
        cmd.arg(format!("-L{}", p.display()));
    }
    // crt 目录也要显式进搜索路径：GNU ld 内置的 SEARCH_DIR 未必覆盖多架构布局，
    // 而 `-lc` 会顺着 libc.so 这条链接脚本摸到 libc_nonshared.a。
    cmd.arg(format!("-L{}", tc.crt_dir.display()));
    if let Some(d) = &tc.libgcc_dir {
        cmd.arg(format!("-L{}", d.display()));
    }
    for lib in req.link_libs {
        if req.whole_archive_libs.contains(lib) {
            cmd.arg("--whole-archive");
        }
        cmd.arg(format!("-l{}", lib));
        if req.whole_archive_libs.contains(lib) {
            cmd.arg("--no-whole-archive");
        }
    }
    // cc 驱动把 `-lgcc` 放在 libc **两侧**：前一次解用户对象里的引用，后一次解
    // libc 内部对 libgcc 的反向引用（如 `__gcc_personality_v0`）。照抄这个顺序。
    if tc.libgcc_dir.is_some() {
        cmd.arg("-lgcc");
    }
    cmd.arg("-lc");
    if let Some(e) = &tc.crtend {
        cmd.arg(e);
    }
    if tc.libgcc_dir.is_some() {
        cmd.arg("-lgcc");
    }
    cmd.arg(&tc.crtn);

    if req.verbose {
        eprintln!("dcc: invoking {}: {:?}", tc.ld, cmd);
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return Ok(Outcome::Unavailable(format!("cannot invoke {}: {}", tc.ld, e)));
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut msg = format!("{} link failed (exit {:?})", tc.ld, out.status.code());
        for chunk in [stderr.trim_end(), stdout.trim_end()] {
            if !chunk.is_empty() {
                msg.push_str(": ");
                msg.push_str(chunk);
            }
        }
        return Err(msg);
    }
    Ok(Outcome::Linked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wl_args_are_split_like_cc_does() {
        assert_eq!(translate_wl("-Wl,-E"), vec!["-E".to_string()]);
        assert_eq!(
            translate_wl("-Wl,-rpath,/opt/lib"),
            vec!["-rpath".to_string(), "/opt/lib".to_string()]
        );
        // 不是 -Wl 的原样保留（防御性：调用点可能混入裸参数）
        assert_eq!(translate_wl("-z"), vec!["-z".to_string()]);
    }

    #[test]
    fn crt_dir_discovery_finds_a_triplet_on_this_host() {
        // 本机（glibc x86_64）必须能找到；找不到说明环境不完整，测试应报出来
        // 而不是静默跳过 —— 直驱链路依赖这一项。
        let (dir, sysroot) = find_crt_dir(None).expect("本机应有 crt1.o/crti.o/crtn.o");
        assert!(sysroot.is_none());
        assert!(dir.join("crt1.o").is_file());
    }

    #[test]
    fn explicit_interp_wins_over_probing() {
        assert_eq!(
            find_interp(Some("/custom/ld.so"), None).as_deref(),
            Some("/custom/ld.so")
        );
    }
}
