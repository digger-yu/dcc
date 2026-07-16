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

fn is_no_parse_file(s: &str) -> bool {
    ends_with(s, ".rlib") || ends_with(s, ".rmeta") || ends_with(s, ".so")
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

fn parse_dep_file(content: &str, target: &str, out: &mut impl Write) -> io::Result<()> {
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
                while !p.is_empty() && !p.starts_with('\n') {
                    if p.starts_with('\\') {
                        p = &p[1..];
                    }
                    p = &p[1..];
                }
                continue;
            }
            b' ' | b'\t' => {
                p = &p[1..];
                continue;
            }
            b'\\' if p.starts_with("\\\n") => {
                p = &p[2..];
                continue;
            }
            b'\n' => {
                p = &p[1..];
                is_target = true;
                continue;
            }
            b':' => {
                p = &p[1..];
                is_target = false;
                is_source = true;
                continue;
            }
            _ => {}
        }

        let mut q = 0;
        while q < p.len() {
            let c = p.as_bytes()[q];
            if c == b' '
                || c == b'\t'
                || c == b'\n'
                || c == b'#'
                || c == b':'
            {
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
        let mut need_parse = false;

        if is_source {
            if !saw_any_target {
                saw_any_target = true;
                writeln!(out, "source_{target} := {token}\n")?;
                writeln!(out, "deps_{target} := \\")?;
                need_parse = true;
            }
        } else if !is_ignored_file(token) {
            let hash = str_hash(token);
            let key = (hash, token.to_string());
            if file_seen.insert(key) {
                writeln!(out, "  {token} \\")?;
                need_parse = true;
            }
        }

        if need_parse && !is_no_parse_file(token) {
            if let Ok(buf) = fs::read_to_string(token) {
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
    parse_dep_file(&dep_content, target, &mut out).unwrap();
    out.flush().unwrap();
}
