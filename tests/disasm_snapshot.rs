// tests/disasm_snapshot.rs - x86_64 反汇编文本快照 (P12-F)
//
// 为什么单独成 target (而不是挂在 integration.rs):
//   P12 方案 §P12-F 的定位是「独立 oracle」: A/B 批次全部建立在编码器自证闭环上,
//   一旦调用点传错寄存器号 / 颠倒 src/dst, encode() 会高高兴兴产出「合法但语义错」
//   的字节 (P11 回归 #6: `mov %ecx,(%rax)` 被错编成 `mov %eax,(%rdx)`)。
//   只有从字节流反推出反汇编文本、与人工 review 过的快照 diff, 才能发现这类回归。
//   本 target 与编码器实现零依赖, 只依赖外部 dcc 二进制 + 主机 objdump。
//
// 用例布局: tests/snapshots/x86_64/<case>/
//   input.c     被测 C 源
//   config.toml 编译参数 (目前只解析 flags 一行, 如 `flags = "-O2 -std=gnu11"`)
//   expected.s  首次人工 review 后冻结的归一化反汇编文本
//
// 归一化规则 (与 P12 方案 §P12-F 步骤 2 一致):
//   1. 行首地址 `   0:` / ` 10:` 一律剥掉 (函数内偏移相对, 无意义)
//   2. 原始字节列 `41 55` 剥掉 (留给 encode 矩阵单测去锁, 文本层只锁语义)
//   3. 函数标题行 `0000000000000000 <name>:` 归一为 `<name>:` (函数名保留,
//      抓「错跳到相邻函数」时函数名是关键证据)
//   4. 行尾注释列 `# 2d <store_d+0x2d>` 剥掉 (纯地址, 不可复现)
//   5. 连续 `nop` (含对齐变体) 折叠成一行 `<nop padding>`
//   6. 保留指令助记符 / 寄存器名 / 内存操作数语义 (参数错位正是要抓的)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 快照用例根目录 (相对 CARGO_MANIFEST_DIR)。
fn snapshots_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("x86_64")
}

/// 判断 token 是否是「原始字节列」的一段: 1-2 位 hex。
fn is_hex_byte(tok: &str) -> bool {
    tok.len() <= 2 && tok.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 把 objdump -d 的原始输出归一化成快照文本。
///
/// 只吃 `.text` 段内的指令行; 文件头 / 节标题 / 其他段一律过滤。
pub(crate) fn normalize_objdump(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_text = false;
    let mut nop_run = 0usize;

    let flush_nop = |out: &mut Vec<String>, run: &mut usize| {
        if *run > 0 {
            out.push("<nop padding>".to_string());
            *run = 0;
        }
    };

    for line in raw.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("Disassembly of section") {
            in_text = trimmed.contains(".text") && !trimmed.contains(".text.");
            continue;
        }
        if !in_text {
            continue;
        }
        // 空行: 不切换 in_text (objdump 节标题后也有空行); 只折叠当前 nop 串
        if trimmed.is_empty() {
            flush_nop(&mut out, &mut nop_run);
            continue;
        }
        // 函数标题行: `0000000000000000 <name>:`
        // objdump 输出行首没有前导空格, 且以 `:` 结尾; 也能匹配到
        // `0000000000000000 <store_d>:` 这类形态。
        if trimmed.ends_with(':') && !trimmed.starts_with(' ') {
            flush_nop(&mut out, &mut nop_run);
            let body = trimmed.trim_start();
            // 剥掉前导 hex 地址 (如 `0000000000000000 `)
            let body = body.trim_start_matches(|c: char| c.is_ascii_hexdigit());
            let body = body.trim_start();
            if let Some(angle) = body.strip_prefix('<') {
                // `<name>:` -> 剥掉尾部 `>` 再剥 `:`
                let name = angle
                    .trim_end_matches(':')
                    .trim_end_matches('>')
                    .trim_end_matches(':');
                out.push(format!("{name}:"));
            } else {
                out.push(trimmed.to_string());
            }
            continue;
        }
        // 指令行: 形如 "   0:\t41 55                   push   r13"
        let Some((_, after_colon)) = trimmed.split_once(':') else {
            continue;
        };
        let after = after_colon.trim_start();
        // 吃掉前导 hex 字节 token, 剩下的就是助记符 + 操作数
        let mut tokens = after.split_whitespace();
        let mut mnemonic: Option<&str> = None;
        let mut rest: Vec<&str> = Vec::new();
        for tok in &mut tokens {
            if mnemonic.is_none() && is_hex_byte(tok) {
                continue;
            }
            if mnemonic.is_none() {
                mnemonic = Some(tok);
            }
            rest.push(tok);
        }
        let Some(mn) = mnemonic else {
            continue;
        };
        let instr = rest.join(" ");
        // 剥掉行尾注释列 (objdump 用 `# 2d <store_d+0x2d>` 标注 RIP-relative)
        let instr = match instr.find(" #") {
            Some(hash) => instr[..hash].trim_end().to_string(),
            None => instr,
        };
        // 归一化跳转目标: `jne 67 <u128_eq+0x67>` -> `jne <u128_eq>`
        // 相对偏移在不同编译间会漂移, 函数名才是可比对的锚点。
        let instr = if mn.starts_with('j') || mn == "call" || mn == "ret" {
            // 形如 `jne 67 <name+0xNN>` 或 `callq 0x0 <foo>`
            if let Some(angle_pos) = instr.find('<') {
                let after_angle = &instr[angle_pos + 1..];
                let end_angle = after_angle.find('>').unwrap_or(after_angle.len());
                let sym = &after_angle[..end_angle];
                // 剥掉 `+0x..` 后缀
                let sym = sym.split('+').next().unwrap_or(sym);
                format!("{} <{}>", mn, sym)
            } else {
                instr
            }
        } else {
            instr
        };

        // 折叠连续 nop
        if mn == "nop" || mn == "nopw" {
            nop_run += 1;
            continue;
        }
        flush_nop(&mut out, &mut nop_run);
        out.push(instr);
    }
    flush_nop(&mut out, &mut nop_run);
    out.join("\n")
}

/// 读取一个用例目录的 config.toml, 返回 flags 字符串 (默认 `-O2 -std=gnu11`)。
fn read_config(case_dir: &Path) -> String {
    let cfg = case_dir.join("config.toml");
    let Ok(text) = fs::read_to_string(&cfg) else {
        return "-O2 -std=gnu11".to_string();
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("flags") {
            if let Some(eq) = rest.strip_prefix('=').map(|s| s.trim()) {
                let unquoted = eq.trim_matches('"').trim_matches('\'');
                return unquoted.to_string();
            }
        }
    }
    "-O2 -std=gnu11".to_string()
}

/// 跑单个用例: 编译 -> objdump -> 归一化 -> 与 expected.s 对比。
fn run_case(name: &str) -> Result<(), String> {
    let case_dir = snapshots_root().join(name);
    let src = case_dir.join("input.c");
    let expected = case_dir.join("expected.s");

    if !src.exists() {
        return Err(format!("{name}: missing input.c"));
    }
    if !expected.exists() {
        return Err(format!("{name}: missing expected.s (run snapshot generator)"));
    }

    let tmp = std::env::temp_dir().join(format!(
        "dcc-snap-{name}-{}.o",
        std::process::id()
    ));
    let flags = read_config(&case_dir);

    let compile = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(flags.split_whitespace())
        .arg("-c")
        .arg("-o")
        .arg(&tmp)
        .arg(&src)
        .output()
        .map_err(|e| format!("{name}: spawn dcc failed: {e}"))?;
    if !compile.status.success() {
        return Err(format!(
            "{name}: dcc compile failed: {}",
            String::from_utf8_lossy(&compile.stderr)
        ));
    }

    let dump = Command::new("objdump")
        .arg("-d")
        .arg(&tmp)
        .output()
        .map_err(|e| format!("{name}: spawn objdump failed: {e}"))?;
    let _ = fs::remove_file(&tmp);
    if !dump.status.success() {
        return Err(format!(
            "{name}: objdump failed: {}",
            String::from_utf8_lossy(&dump.stderr)
        ));
    }

    let got = normalize_objdump(&String::from_utf8_lossy(&dump.stdout));
    let want = fs::read_to_string(&expected)
        .map_err(|e| format!("{name}: read expected.s failed: {e}"))?;
    let want = want.trim_end();
    let got = got.trim_end();

    if got == want {
        Ok(())
    } else {
        Err(format!(
            "{name}: snapshot mismatch\n--- expected ({expected:?})\n+++ actual\n{got}\n--- diff (expected vs actual) ---\n{}",
            unified_diff(want, got)
        ))
    }
}

/// 极简行级 diff: 按行对齐, 输出「< expected / > actual」格式。
fn unified_diff(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < w.len() || j < g.len() {
        if i < w.len() && j < g.len() && w[i] == g[j] {
            i += 1;
            j += 1;
            continue;
        }
        // 找下一个对齐点 (最多向前看 4 行)
        let mut found = None;
        'outer: for di in 0..=4 {
            for dj in 0..=4 {
                if di == 0 && dj == 0 {
                    continue;
                }
                if i + di < w.len() && j + dj < g.len() && w[i + di] == g[j + dj] {
                    found = Some((di, dj));
                    break 'outer;
                }
            }
        }
        match found {
            Some((di, dj)) => {
                for k in 0..di {
                    out.push_str(&format!("< {}\n", w[i + k]));
                }
                for k in 0..dj {
                    out.push_str(&format!("> {}\n", g[j + k]));
                }
                i += di;
                j += dj;
            },
            None => {
                if i < w.len() {
                    out.push_str(&format!("< {}\n", w[i]));
                    i += 1;
                }
                if j < g.len() {
                    out.push_str(&format!("> {}\n", g[j]));
                    j += 1;
                }
            },
        }
    }
    out
}

/// 用例清单: 与 tests/snapshots/x86_64/<name>/ 一一对应。
///
/// 新增用例时: (1) 建目录 + input.c + config.toml;
///             (2) 用当前 HEAD 的 dcc 编译, 归一化后写入 expected.s;
///             (3) 人工 review 后把名字加进下面列表。
const CASES: &[&str] = &[
    // --- P11 已修复回归现场 (7 个) ---
    "movsd_store",
    "bitfield_shl",
    "movd_int_xmm",
    "simd_mov_sib",
    "bitfield_shr",
    "bitfield_inc",
    "uint128_eq",
    // --- 日常热点 (13 个) ---
    "func_prologue",
    "arithmetic_basic",
    "memory_load",
    "if_else_loop",
    "float_basic",
    "struct_passing",
    "global_riprelative",
    "va_arg_simple",
    "nested_func",
    "simd_128",
    "long_double",
    "atomic_basic",
    "func_ptr_call",
];

#[test]
fn snapshot_cases_all_pass() {
    let root = snapshots_root();
    let mut failures = Vec::new();
    for name in CASES {
        match run_case(name) {
            Ok(()) => {},
            Err(e) => failures.push(e),
        }
    }
    let on_disk = fs::read_dir(&root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0);
    assert!(
        on_disk >= CASES.len(),
        "snapshot dir {root:?} has only {on_disk} cases, CASES lists {}",
        CASES.len()
    );
    if !failures.is_empty() {
        panic!(
            "{} snapshot case(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
