//! Kbuild fixdep 的 Rust 实现，替代 dcc 编译的 C fixdep（宿主 printf 尚不可用）。
//! 用法：fixdep <depfile> <target> <cmdline>

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, Write};

fn usage() -> ! {
    eprintln!("Usage: fixdep <depfile> <target> <cmdline>");
    std::process::exit(1);
}

fn str_hash(s: &str) -> u32 {
    let mut hash: u32 = 2166136261;
    for b in s.bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn ends_with(s: &str, suffix: &str) -> bool {
    s.ends_with(suffix)
}

fn is_ignored_file(s: &str) -> bool {
    ends_with(s, "include/generated/autoconf.h")
}

/// 系统/宿主路径不可能包含内核 CONFIG_*，跳过以避免不必要的文件读取
fn is_system_path(s: &str) -> bool {
    s.starts_with("/usr/") || s.starts_with("/lib/")
}

fn is_no_parse_file(s: &str) -> bool {
    ends_with(s, ".rlib") || ends_with(s, ".rmeta") || ends_with(s, ".so")
}

/// 反转义依赖文件中的路径 (GCC 用 `\` 转义空格/`#`/`:` 等).
pub fn unescape_dep_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.clone().next().as_ref() {
                if next != '\n' {
                    out.push(chars.next().unwrap());
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

fn parse_config_file(content: &str, config_seen: &mut HashSet<String>) {
    let bytes = content.as_bytes();
    let mut i = 0;
    while i + 7 <= bytes.len() {
        if &bytes[i..i + 7] != b"CONFIG_" {
            i += 1;
            continue;
        }
        if i > 0 {
            let prev = bytes[i - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                i += 7;
                continue;
            }
        }
        let start = i + 7;
        let mut end = start;
        while end < bytes.len() {
            let c = bytes[end];
            if c.is_ascii_alphanumeric() || c == b'_' {
                end += 1;
            } else {
                break;
            }
        }
        if end > start {
            let mut sym_end = end;
            if end - start > 7 && &bytes[end - 7..end] == b"_MODULE" {
                sym_end = end - 7;
            }
            if sym_end > start {
                let sym = std::str::from_utf8(&bytes[start..sym_end]).unwrap_or("");
                if config_seen.insert(sym.to_string()) {
                    println!("    $(wildcard include/config/{sym}) \\");
                }
            }
        }
        i = end;
    }
}

pub fn parse_dep_file(content: &str, target: &str, out: &mut impl Write) -> io::Result<()> {
    let mut p = content;
    let mut saw_any_target = false;
    let mut is_target = true;
    let mut is_source = false;
    let mut file_seen: HashSet<(u32, String)> = HashSet::new();
    let mut config_seen: HashSet<String> = HashSet::new();

    while !p.is_empty() {
        let ch = p.as_bytes()[0];
        match ch {
            b'#' => {
                p = &p[1..];
                // 注释不影响状态: 跳过整行 (含续行), 恢复之前的 is_target/is_source
                let was_target = is_target;
                let was_source = is_source;
                while !p.is_empty() {
                    if p.starts_with("\\\n") {
                        p = &p[2..];
                        continue;
                    }
                    if p.starts_with('\n') {
                        p = &p[1..];
                        break;
                    }
                    p = &p[1..];
                }
                // 恢复注释前的状态 (若在续行中则保持依赖列表状态)
                is_target = was_target;
                is_source = was_source;
                continue;
            },
            b' ' | b'\t' => {
                p = &p[1..];
                continue;
            },
            b'\\' if p.starts_with("\\\n") => {
                p = &p[2..];
                continue;
            },
            b'\n' => {
                p = &p[1..];
                is_target = true;
                continue;
            },
            b':' => {
                p = &p[1..];
                is_target = false;
                is_source = true;
                continue;
            },
            _ => {},
        }

        let mut q = 0;
        while q < p.len() {
            let c = p.as_bytes()[q];
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'#' || c == b':' {
                break;
            }
            if c == b'\\' {
                if q + 1 < p.len() && p.as_bytes()[q + 1] == b'\n' {
                    break;
                }
                q += 1;
            }
            q += 1;
        }

        if is_target {
            p = &p[q..];
            continue;
        }

        let token = &p[..q];
        // GCC 用 `\` 转义路径中的特殊字符, 读取文件前需反转义
        let real_path = unescape_dep_path(token);
        let mut need_parse = false;

        if is_source {
            if !saw_any_target {
                saw_any_target = true;
                writeln!(out, "source_{target} := {real_path}\n")?;
                writeln!(out, "deps_{target} := \\")?;
                need_parse = true;
            }
        } else if !is_ignored_file(&real_path) {
            let hash = str_hash(&real_path);
            let key = (hash, real_path.clone());
            if file_seen.insert(key) {
                writeln!(out, "  {real_path} \\")?;
                need_parse = true;
            }
        }

        if need_parse && !is_no_parse_file(&real_path) && !is_system_path(&real_path) {
            if let Ok(buf) = fs::read_to_string(&real_path) {
                parse_config_file(&buf, &mut config_seen);
            }
        }

        is_source = false;
        p = &p[q..];
    }

    if !saw_any_target {
        eprintln!("fixdep: parse error; no targets found");
        std::process::exit(1);
    }

    writeln!(out, "\n{target}: $(deps_{target})\n")?;
    writeln!(out, "$(deps_{target}):")?;
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        usage();
    }
    let depfile = &args[1];
    let target = &args[2];
    let cmdline = &args[3];

    let dep_content = fs::read_to_string(depfile).unwrap_or_else(|e| {
        eprintln!("fixdep: error opening file: {depfile}: {e}");
        std::process::exit(2);
    });

    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "savedcmd_{target} := {cmdline}\n").unwrap();
    parse_dep_file(&dep_content, target, &mut out).unwrap_or_else(|e| {
        eprintln!("fixdep: {e}");
        std::process::exit(1);
    });
    out.flush().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_no_escapes() {
        assert_eq!(unescape_dep_path("/usr/include/stdio.h"), "/usr/include/stdio.h");
    }

    #[test]
    fn unescape_space() {
        assert_eq!(unescape_dep_path("/my\\ path/file.h"), "/my path/file.h");
    }

    #[test]
    fn unescape_hash() {
        assert_eq!(unescape_dep_path("/a\\#b.h"), "/a#b.h");
    }

    #[test]
    fn unescape_colon() {
        assert_eq!(unescape_dep_path("/a\\:b.h"), "/a:b.h");
    }

    #[test]
    fn unescape_multiple() {
        assert_eq!(unescape_dep_path("/a\\ b/c\\#d/e\\:f.h"), "/a b/c#d/e:f.h");
    }

    #[test]
    fn parse_dep_file_gcc_format() {
        // GCC 格式: target: source \ \n dep1 \ \n dep2
        let input = "foo.o: foo.c \\\n  bar.h \\\n  baz.h\n";
        let mut out = Vec::new();
        parse_dep_file(input, "foo.o", &mut out).unwrap();
        let result = String::from_utf8(out).unwrap();
        assert!(result.contains("source_foo.o := foo.c"));
        assert!(result.contains("bar.h"));
        assert!(result.contains("baz.h"));
    }

    #[test]
    fn parse_dep_file_escaped_paths() {
        // 转义空格的路径
        let input = "foo.o: my\\ src.c \\\n  my\\ header.h\n";
        let mut out = Vec::new();
        parse_dep_file(input, "foo.o", &mut out).unwrap();
        let result = String::from_utf8(out).unwrap();
        // 反转义后应为实际路径
        assert!(result.contains("my src.c"));
        assert!(result.contains("my header.h"));
    }
}
