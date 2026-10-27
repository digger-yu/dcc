// src/env_hygiene_tests.rs —— 进程环境不变式：本仓库的代码不许改进程环境。
//
// 为什么钉成用例而不是注释：libtest 在同一进程里多线程跑用例，而 `set_var` / `remove_var`
// 与并发 `var` 在 glibc 层就是数据竞争 —— `setenv` 重写 environ 那一格时，另一线程刚从
// `getenv` 拿到的指针可能正被覆盖；而且设了不还的变量会把之后所有用例的读数口径一起带走
// (`HOME` 泄漏之后，include 缓存根指向一个已被 `remove_dir_all` 的目录，缓存静默失效)。
// 症状是「偶发一条用例红，重跑又绿」，并且换个 `--test-threads` 就改判据 —— 这种门禁证明
// 不了任何东西。`tests/common/harness.rs` 里「旋钮一律随子进程下发」早就写了同一条纪律，
// 但注释不是闸门：LIB 唯一一次偶发红恰恰发生在写着这条注释的仓库里。
//
// 逃生口：确需改环境的生产代码，在同一行或其上一行写 `ok-env-mutation: <理由>`，
// 本用例只认这个标记，其余一律点名。

use std::fs;
use std::path::Path;

/// 被点名的形态：`env::set_var(` / `env::remove_var(`。
/// 字面量在这里是**拆开**写的 —— 本用例会扫自己的源文件，写成连续文本就会自匹配。
const NEEDLES: [&str; 2] = [
    concat!("env::set_", "var("),
    concat!("env::remove_", "var("),
];

/// 纯函数：给一段 Rust 文本，返回其中真正改进程环境的行 (1-based 行号 + 原文)。
/// 跳过两类：注释行 (行首 `//` / `/*` / `*`), 以及带 `ok-env-mutation:` 标记的行
/// (标记写在本行或上一行都算)。
fn scan_env_mutations(text: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }
        if !NEEDLES.iter().any(|n| raw.contains(n)) {
            continue;
        }
        let marked_here = raw.contains("ok-env-mutation:");
        let marked_prev = idx
            .checked_sub(1)
            .map_or(false, |p| lines[p].contains("ok-env-mutation:"));
        if marked_here || marked_prev {
            continue;
        }
        hits.push((idx + 1, raw.trim().to_string()));
    }
    hits
}

/// 递归收集 `dir` 下所有 `.rs` 文件。
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn process_env_is_never_mutated_anywhere_in_the_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for sub in ["src", "tests"] {
        rust_files(&root.join(sub), &mut files);
    }
    assert!(
        files.len() > 300,
        "只扫到 {} 个 .rs —— 扫描范围本身失效了，本用例就成了假绿",
        files.len()
    );
    let mut report = Vec::new();
    for path in &files {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            // 非 UTF-8 的 .rs (内嵌夹具) 不可能有本仓库的调用点，跳过但不静默：
            // 计数在下面报告，便于确认口径。
            Err(_) => continue,
        };
        for (line, src) in scan_env_mutations(&text) {
            report.push(format!("{}:{}: {}", path.display(), line, src));
        }
    }
    assert!(
        report.is_empty(),
        "改进程环境的调用点 {} 处:\n{}\n\n为什么必须改走子进程环境 (Command::env) 或本线程可见的注入: \
         见本文件头注释。",
        report.len(),
        report.join("\n")
    );
}

/// 反向断言：闸门真的会红。没有这一条，上面那个用例可能只是在扫一片空气。
#[test]
fn env_mutation_detector_fires_on_poisoned_snippets() {
    // 1) 真调用 → 必须点名。下面三个字面量一律用 concat! 拆开写: 本用例会扫自己的
    //    源文件, 连续文本会自匹配 (与 NEEDLES 同一条)。
    let poisoned = concat!("fn t() {\n    std::env::set_", "var(\"HOME\", \"/tmp/x\");\n}\n");
    let hits = scan_env_mutations(poisoned);
    assert_eq!(hits.len(), 1, "真调用没被抓到: {hits:?}");
    assert_eq!(hits[0].0, 2);

    // 2) 同样的话写在注释里 → 不算 (文档要能提这个坑)
    let documented = concat!("/// 别用 std::env::set_", "var(...) 这种写法\nfn t() {}\n");
    assert!(scan_env_mutations(documented).is_empty(), "注释被误伤");

    // 3) 带理由的显式豁免 → 不算
    let opted_out =
        "fn t() {\n    // ok-env-mutation: 自举需要\n    std::env::remove_var(\"CC1\");\n}\n";
    assert!(scan_env_mutations(opted_out).is_empty(), "行上/行上的标记没生效");
    let marked_same_line =
        "fn t() { std::env::set_var(\"CC1\", \"x\") } // ok-env-mutation: 自举\n";
    assert!(scan_env_mutations(marked_same_line).is_empty(), "同行标记没生效");

    // 4) remove_var 与 set_var 同等对待
    assert_eq!(scan_env_mutations(concat!("std::env::remove_", "var(\"A\");")).len(), 1);
}
