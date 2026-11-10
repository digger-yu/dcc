//! 编译探针与读数助手：按旗标/环境变量把夹具编成可执行文件或对象，再从产物字节、
//! `.s` 文本或 `readelf` 输出里取判据需要的读数。
//!
//! 由 `tests/common/harness.rs` 以 `#[path]` 挂载并 `pub(crate) use` 重导出。
//! 这些助手原先散在 `tests/integration/` 的主题子文件里，而 §14 要求主题子文件只含
//! `#[test] fn`（`scripts/gen_coverage_index.sh` 的 STRUCT-HELPER 会拦），所以统一
//! 收到这里。类别按「助手自己做什么」定，不按调用方最终证明了什么定：编译/取产物/
//! 解析产物的记 compile，真的把产物跑起来的记 run，只做源文本包装的记 aux。

use std::fs;
use std::process::Command;

use super::{
    compile_asm_text_with_env, run_compile_and_run_env, run_compile_object_env, run_exe, temp_tag,
};

/// F4/F5/F6 smoke-compile（只编译到对象，不链接：夹具引用外部 `pick()`
/// 或依赖 x87 `_Imaginary` lowering，本来就没有可链接的 main）。
pub(crate) fn compile_only(name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir();
    let tag = temp_tag();
    let src_path = dir.join(format!("dcc-{name}-{tag}.c"));
    let obj_path = dir.join(format!("dcc-{name}-{tag}.o"));
    fs::write(&src_path, src).expect("write fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-std=gnu23", "-O2", "-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&obj_path);
    out
}

/// 汇编文本里「有一行的整条指令就是它」（去掉前后空白与行内注释后逐字比）。
/// 用子串判会假阳也假阴：临时文件名、`.file` 指令里都可能出现同样的字母。
pub(crate) fn has_insn_line(asm_text: &str, mnemonic: &str) -> bool {
    asm_text.lines().any(|l| {
        let l = l.trim();
        let l = l.split_once("/*").map(|(a, _)| a).unwrap_or(l);
        l.trim() == mnemonic
    })
}

/// 判一个未定义符号名是不是「没解开的数字标签引用」（`1b`、`771f` 这种）。
/// 写在这里而不是拉一个 regex 依赖：判据就是一条字符规则，要一眼能看完。
pub(crate) fn regex_like_numeric_ref(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    let last = bytes[bytes.len() - 1];
    (last == b'b' || last == b'f') && bytes[..bytes.len() - 1].iter().all(|c| c.is_ascii_digit())
}

/// 强制函数落进「非结构化通道」的序章：一张跳表 + 一条永不为真的 `goto *表[0]`。
const CG_PRE: &str = r#"
    extern void exit(int);
    static const void *cg_dt[] = {&&cgA, &&cgB};
    int cg_seed;
"#;

/// 把一段计算 `return` 值的函数体包成「带计算转移的 main」。
pub(crate) fn cg_wrap(body: &str) -> String {
    format!(
        r#"
{CG_PRE}
    int main(void) {{
        if (cg_seed < 0) {{ goto *cg_dt[0]; }}
        cgA: ;
{body}
        cgB: ;
        return 0;
    }}
"#
    )
}

/// 冷格外置的公共夹具：同一段源码在旋钮关/开两种档位下分别编译并运行。
///
/// 返回 `(rc_off, rc_on, out_off, out_on, bytes_off, bytes_on)`。值判据要求前四者两两
/// 相等；机制判据由调用方对后两者主张 `!=`（该动的动了）或 `==`（不该动的没动）。
pub(crate) fn cold_outline_pair(
    name: &str,
    source: &str,
) -> (i32, i32, String, String, Vec<u8>, Vec<u8>) {
    let on: &[(&str, &str)] = &[("DCC_COLD_OUTLINE", "1")];
    let (out_off, exe_off) = run_compile_and_run_env(name, source, &["-O2"], &[]);
    let (out_on, exe_on) = run_compile_and_run_env(name, source, &["-O2"], on);
    assert!(
        out_off.status.success() && out_on.status.success(),
        "编译失败 off={} on={}",
        String::from_utf8_lossy(&out_off.stderr),
        String::from_utf8_lossy(&out_on.stderr)
    );
    let (o1, b1) = run_compile_object_env(name, source, "-O2", &[]);
    let (o2, b2) = run_compile_object_env(name, source, "-O2", on);
    assert!(o1.status.success() && o2.status.success(), "对象编译失败");
    // 夹具用 `exit(计算值)` 回传结果 ⇒ 非零退出码是正常现象，不能拿 `status.success()`
    // 当「跑对了」；只拒接硬崩（拿不到退出码 = 被信号打死）。
    let rc = |p: &std::path::PathBuf| {
        let o = Command::new(p).output().expect("run fixture");
        assert!(
            o.status.code().is_some(),
            "夹具被信号打死: {:?} {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr)
        );
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
        )
    };
    let (rc_off, out_off) = rc(&exe_off);
    let (rc_on, out_on) = rc(&exe_on);
    let _ = fs::remove_file(&exe_off);
    let _ = fs::remove_file(&exe_on);
    (rc_off, rc_on, out_off, out_on, b1, b2)
}

/// 值判据：两档必须给出完全相同的退出码与 stdout。
pub(crate) fn assert_cold_same_value(
    t: &(i32, i32, String, String, Vec<u8>, Vec<u8>),
    what: &str,
) {
    let (rc_off, rc_on, out_off, out_on) = (t.0, t.1, t.2.clone(), t.3.clone());
    assert_eq!(rc_off, rc_on, "{what}: 旋钮开后的退出码与关时不同（{rc_off} vs {rc_on}）");
    assert_eq!(out_off, out_on, "{what}: 旋钮开后的 stdout 与关时不同");
}

/// 形参当家（`DCC_HOME_PARAMS`）两档编译+运行，返回 `(rc_off, rc_on, out_off, out_on)`。
///
/// `off` 臂显式带 `DCC_HOME_PARAMS=0`：不显式写就等于把判据交给外部环境
/// （整套用例带 `=1` 跑时两臂会同形，判据空跑）。
pub(crate) fn param_home_run(name: &str, src: &str) -> (i32, i32, String, String) {
    let off: &[(&str, &str)] = &[("DCC_HOME_PARAMS", "0")];
    let on: &[(&str, &str)] = &[("DCC_HOME_PARAMS", "1")];
    let (o1, e1) = run_compile_and_run_env(name, src, &["-O2"], off);
    let (o2, e2) = run_compile_and_run_env(name, src, &["-O2"], on);
    assert!(o1.status.success() && o2.status.success(), "编译失败");
    let go = |p: &std::path::PathBuf| {
        let out = Command::new(p).output().expect("run fixture");
        assert!(
            out.status.code().is_some(),
            "夹具被信号打死: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let a = go(&e1);
    let b = go(&e2);
    let _ = fs::remove_file(&e1);
    let _ = fs::remove_file(&e2);
    (a.0, b.0, a.1, b.1)
}

/// 两档编到对象，返回字节（结构判据用）。off 同样显式写 `=0`。
pub(crate) fn param_home_objects(name: &str, src: &str) -> (Vec<u8>, Vec<u8>) {
    let (_o1, b1) = run_compile_object_env(name, src, "-O2", &[("DCC_HOME_PARAMS", "0")]);
    let (_o2, b2) = run_compile_object_env(name, src, "-O2", &[("DCC_HOME_PARAMS", "1")]);
    (b1, b2)
}

/// S1 的语料：仓库里最大的一份自包含 C 源（计算转移 + 多帧槽）。
const S1_CORPUS: &str = "tests/fixtures/c/bootstrap_selftest.c";

/// 用指定的 `DCC_BLOCKOPT_RULES` 位图把 S1 语料编成汇编文本。
pub(crate) fn compile_s1_with(rules: &str) -> (String, String) {
    let src = std::fs::read_to_string(S1_CORPUS)
        .unwrap_or_else(|e| panic!("读语料 {S1_CORPUS} 失败：{e}"));
    compile_asm_text_with_env("s1-corpus", &src, &["-O2"], &[("DCC_BLOCKOPT_RULES", rules)])
}

/// 数一份汇编里的**帧载入**次数。
///
/// 为什么不能用 `grep -c r13`：`mov %rax, -0x30(%r13)` 是存出，把它计进来会让
/// 「转发掉一次载入」这件事读数不动 —— 判据会对着真正的那一半失效。
pub(crate) fn frame_loads_in_asm(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with("mov")
                || l.starts_with("add")
                || l.starts_with("sub")
                || l.starts_with("cmp")
                || l.starts_with("test")
                || l.starts_with("lea")
        })
        // 载入 = 带 `(%r13)` 的那一侧在**逗号前**（源）。AT&T 是 `src, dst`。
        .filter(|l| match l.find(',') {
            Some(i) => l[..i].contains("(%r13"),
            None => false,
        })
        .count()
}

/// 数一份汇编里的指令条数（只认小写助记符开头的非标签/非伪指令行）。
pub(crate) fn insn_count(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with('.')
                && !l.starts_with('#')
                && l.chars().next().map_or(false, |c| c.is_ascii_lowercase())
        })
        .count()
}

/// 编译到可重定位对象（多个 flag + env 旋钮），拿字节回来交给下面两个 readelf 助手。
///
/// 为什么不走 `run_compile_object_env`：它只接一个优化参数，而本族用例同时需要 `-O2`
/// 与 `-g` 两个（loclist 只在 -O2 的家分配之后才有意义）。
pub(crate) fn region_obj_with_env(
    name: &str,
    source: &str,
    flags: &[&str],
    envs: &[(&str, &str)],
) -> Vec<u8> {
    let dir = std::env::temp_dir();
    let tag = temp_tag();
    let src = dir.join(format!("dcc-{name}-{tag}.c"));
    let obj = dir.join(format!("dcc-{name}-{tag}.o"));
    fs::write(&src, source).expect("write fixture");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dcc"));
    cmd.args(["-std=gnu23"]).args(flags).args(["-c"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.args(["-o"]).arg(&obj).arg(&src).output().expect("run dcc");
    assert!(out.status.success(), "编译失败: {}", String::from_utf8_lossy(&out.stderr));
    let bytes = fs::read(&obj).unwrap_or_default();
    let _ = fs::remove_file(src);
    let _ = fs::remove_file(obj);
    bytes
}

/// 一个对象的节名清单（写成一行，给 `contains(".debug_loc")` 这种存在性判据用）。
///
/// `readelf -SW` 的每一行形如 `  [13] .debug_loc PROGBITS ...` ⇒ 节名是第二个字段；
/// 表头/空行拿不到以 `.` 开头的字段，自然被滤掉。
pub(crate) fn elf_section_names(obj: &[u8]) -> String {
    let text = elf_readelf(obj, &["-SW"]);
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let _ = it.next()?;
            it.next()
        })
        .filter(|n| n.starts_with('.'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 把某个节 dump 成人读文本（用于「能不能解」这一类结构判据）。
pub(crate) fn elf_dump_section(obj: &[u8], sec: &str) -> String {
    let name = sec.trim_start_matches('.');
    elf_readelf(obj, &["--wide", "--debug-dump", &format!("{name}")])
}

/// 把对象字节落到临时文件喂给 `readelf`，返回 stdout 文本。
pub(crate) fn elf_readelf(obj: &[u8], args: &[&str]) -> String {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "dcc-readelf-{}-{}.o",
        temp_tag(),
        args.join("-").replace(['/', '.'], "_")
    ));
    fs::write(&path, obj).expect("write obj for readelf");
    let out = Command::new("readelf").args(args).arg(&path).output();
    let _ = fs::remove_file(&path);
    String::from_utf8_lossy(&out.expect("run readelf").stdout).into_owned()
}

/// 编译 + 运行 + 取退出码（带 `DCC_BLOCKOPT_RULES` 下发的那一档）。
pub(crate) fn run_exe_and_check(name: &str, source: &str, rules: &str) -> i32 {
    let (o, e) = run_compile_and_run_env(name, source, &["-O2"], &[("DCC_BLOCKOPT_RULES", rules)]);
    assert!(o.status.success(), "{name} 编译失败: {}", String::from_utf8_lossy(&o.stderr));
    run_exe(&e)
}

/// 按 `-S` 文本逐行累加 rsp 偏移，返回每个 `call` 处的偏移。
///
/// 起点取 8：ABI 规定 `call` 刚执行的函数入口 rsp ≡ 8 (mod 16)。只认本后端
/// 对「改变 rsp」的四种写法（`push`/`pop`/`sub $N, %rsp`/`add $N, %rsp`）。
/// 遇到 `mov %r13, %rsp` 这类把 rsp 拉回帧基的复位就不再累加 —— 判据只对
/// 累加段成立，含糊的地方不猜（实测目标形状里没有它）。
pub(crate) fn call_site_rsp_offsets(asm: &str) -> Vec<i64> {
    let mut off = 8i64;
    let mut calls = Vec::new();
    for line in asm.lines() {
        let l = line.trim();
        if l == "mov %r13, %rsp" {
            break;
        }
        if let Some(n) = l.strip_prefix("sub $").and_then(|r| r.strip_suffix(", %rsp")) {
            off -= n.trim().parse::<i64>().expect("sub rsp 立即数");
        } else if let Some(n) = l.strip_prefix("add $").and_then(|r| r.strip_suffix(", %rsp")) {
            off += n.trim().parse::<i64>().expect("add rsp 立即数");
        } else if l.starts_with("push ") {
            off -= 8;
        } else if l.starts_with("pop ") {
            off += 8;
        } else if l.starts_with("call ") {
            calls.push(off);
        }
    }
    calls
}
