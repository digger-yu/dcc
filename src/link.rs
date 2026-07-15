// src/link.rs - 可重定位目标文件链接（-r）与 LTO 摘要合并
// 第一性原理: 链接期读取各 TU 的 .llvm.lto，合并后写入输出 .o。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diagnostics::{Diagnostic, Span};
use crate::lto;

/// 合并多个 ET_REL 目标文件；启用 LTO 时合并 .llvm.lto 并注入输出节。
pub fn link_relocatable(
    inputs: &[PathBuf],
    output: &Path,
    lto_enabled: bool,
    verbose: bool,
) -> Result<(), Vec<Diagnostic>> {
    if inputs.is_empty() {
        return Err(vec![Diagnostic::err(
            "E0200",
            "no input files for link",
            Span::point("", 0, 0),
        )]);
    }
    let object_bytes: Vec<Vec<u8>> = inputs
        .iter()
        .map(|p| {
            fs::read(p).map_err(|e| {
                Diagnostic::err(
                    "E0201",
                    format!("cannot read {}: {}", p.display(), e),
                    Span::point(&p.display().to_string(), 0, 0),
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let refs: Vec<&[u8]> = object_bytes.iter().map(|b| b.as_slice()).collect();

    let lto_unit = if lto_enabled {
        Some(lto::collect_from_objects(&refs))
    } else {
        None
    };

    if verbose {
        if let Some(ref unit) = lto_unit {
            eprintln!(
                "dcc: LTO merge {} functions from {} objects",
                unit.functions.len(),
                inputs.len()
            );
            for name in lto::dead_local_functions(unit) {
                eprintln!("dcc: LTO dead local: {}", name);
            }
        }
    }

    let mut ld = Command::new("ld");
    ld.arg("-r").arg("-o").arg(output);
    for input in inputs {
        ld.arg(input);
    }
    let status = ld.status().map_err(|e| {
        vec![Diagnostic::err(
            "E0202",
            format!("cannot invoke ld: {}", e),
            Span::point("", 0, 0),
        )]
    })?;
    if !status.success() {
        return Err(vec![Diagnostic::err(
            "E0202",
            "ld -r failed",
            Span::point("", 0, 0),
        )]);
    }

    if let Some(unit) = lto_unit {
        inject_lto_section(output, &lto::serialize_summary(&unit))?;
    }

    Ok(())
}

/// 用 objcopy 向 ET_REL 输出追加 .llvm.lto 节（Linux 链接路径）。
fn inject_lto_section(output: &Path, payload: &[u8]) -> Result<(), Vec<Diagnostic>> {
    let tmp = std::env::temp_dir().join(format!("dcc-lto-{}.bin", std::process::id()));
    fs::write(&tmp, payload).map_err(|e| {
        vec![Diagnostic::err(
            "E0203",
            format!("cannot write LTO temp: {}", e),
            Span::point("", 0, 0),
        )]
    })?;
    let tmp_str = tmp.to_string_lossy();
    let status = Command::new("objcopy")
        .arg("--add-section")
        .arg(format!(".llvm.lto={}", tmp_str))
        .arg(output)
        .status()
        .map_err(|e| {
            vec![Diagnostic::err(
                "E0203",
                format!("cannot invoke objcopy for LTO section: {}", e),
                Span::point("", 0, 0),
            )]
        })?;
    let _ = fs::remove_file(&tmp);
    if !status.success() {
        return Err(vec![Diagnostic::err(
            "E0203",
            "objcopy failed to inject .llvm.lto",
            Span::point("", 0, 0),
        )]);
    }
    Ok(())
}
