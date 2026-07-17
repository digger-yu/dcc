// src/link.rs - 可重定位目标文件链接（-r）与 LTO 摘要合并
//
// 设计边界（非自研链接器）：
// - 合并 .o 委托 host `ld -r`；注入/剥离 .llvm.lto 与 dead symbol 用 `objcopy`
// - 不做 IR 级跨 TU 内联/常量传播；LTO 仅摘要 + --gc-sections + strip
// - 可通过 LD / OBJCOPY 环境变量覆盖工具路径

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diagnostics::{Diagnostic, Span};
use crate::lto;

/// 解析 host 工具路径（`LD` / `OBJCOPY` 环境变量优先）。
fn tool_path(default: &str, env_var: &str) -> PathBuf {
    std::env::var(env_var)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn tool_command(default: &str, env_var: &str) -> Command {
    Command::new(tool_path(default, env_var))
}

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
                vec![Diagnostic::err(
                    "E0201",
                    format!("cannot read {}: {}", p.display(), e),
                    Span::point(&p.display().to_string(), 0, 0),
                )]
            })
        })
        .collect::<Result<Vec<_>, Vec<Diagnostic>>>()?;
    let refs: Vec<&[u8]> = object_bytes.iter().map(|b| b.as_slice()).collect();

    let mut lto_unit = if lto_enabled {
        Some(lto::collect_from_objects(&refs))
    } else {
        None
    };

    let mut dead_syms: Vec<String> = vec![];
    if let Some(ref mut unit) = lto_unit {
        lto::mark_references_from_objects(unit, &refs);
        dead_syms = lto::dead_local_functions(unit);
        if unit.path.is_empty() {
            unit.path = output.display().to_string();
        }
    }

    if verbose {
        if let Some(ref unit) = lto_unit {
            eprintln!(
                "dcc: LTO merge {} functions from {} objects",
                unit.functions.len(),
                inputs.len()
            );
            for name in &dead_syms {
                eprintln!("dcc: LTO dead local: {}", name);
            }
        }
        eprintln!(
            "dcc: link {} -> {} (ld={}, objcopy={})",
            inputs.len(),
            output.display(),
            tool_path("ld", "LD").display(),
            tool_path("objcopy", "OBJCOPY").display(),
        );
    }

    let mut ld = tool_command("ld", "LD");
    ld.arg("-r").arg("-o").arg(output);
    if lto_enabled {
        ld.arg("--gc-sections");
        // bfd ld 要求 gc 根：-u 保留至少一个已定义符号
        if let Some(ref unit) = lto_unit {
            if let Some(root) = lto::gc_sections_root(unit, &dead_syms) {
                ld.arg("-u").arg(&root);
            }
        }
    }
    for input in inputs {
        ld.arg(input);
    }
    let ld_path = tool_path("ld", "LD");
    let status = ld.status().map_err(|e| {
        vec![Diagnostic::err(
            "E0202",
            format!(
                "cannot invoke {}: {} (set LD= or install binutils)",
                ld_path.display(),
                e
            ),
            Span::point("", 0, 0),
        )]
    })?;
    if !status.success() {
        return Err(vec![Diagnostic::err(
            "E0202",
            format!("{} -r failed (exit {:?})", ld_path.display(), status.code()),
            Span::point("", 0, 0),
        )]);
    }

    if let Some(unit) = lto_unit {
        inject_lto_section(output, &lto::serialize_summary(&unit))?;
    }

    if lto_enabled && !dead_syms.is_empty() {
        strip_dead_symbols(output, &dead_syms)?;
    }

    Ok(())
}

/// 用 objcopy 剔除 LTO 探测到的 dead local 符号（--gc-sections 的补充）。
fn strip_dead_symbols(output: &Path, names: &[String]) -> Result<(), Vec<Diagnostic>> {
    let objcopy = tool_path("objcopy", "OBJCOPY");
    let mut cmd = tool_command("objcopy", "OBJCOPY");
    for name in names {
        cmd.arg(format!("--strip-symbol={}", name));
    }
    cmd.arg(output);
    let status = cmd.status().map_err(|e| {
        vec![Diagnostic::err(
            "E0204",
            format!(
                "cannot invoke {} for dead symbol strip: {} (set OBJCOPY=)",
                objcopy.display(),
                e
            ),
            Span::point("", 0, 0),
        )]
    })?;
    if !status.success() {
        return Err(vec![Diagnostic::err(
            "E0204",
            format!("{} failed to strip dead LTO symbols", objcopy.display()),
            Span::point("", 0, 0),
        )]);
    }
    Ok(())
}

/// 用 objcopy 向 ET_REL 输出追加 .llvm.lto 节（Linux 链接路径）。
fn inject_lto_section(output: &Path, payload: &[u8]) -> Result<(), Vec<Diagnostic>> {
    let objcopy = tool_path("objcopy", "OBJCOPY");
    let tmp = std::env::temp_dir().join(format!("dcc-lto-{}.bin", std::process::id()));
    fs::write(&tmp, payload).map_err(|e| {
        vec![Diagnostic::err(
            "E0203",
            format!("cannot write LTO temp: {}", e),
            Span::point("", 0, 0),
        )]
    })?;
    let tmp_str = tmp.to_string_lossy();
    let status = tool_command("objcopy", "OBJCOPY")
        .arg("--update-section")
        .arg(format!(".llvm.lto={}", tmp_str))
        .arg(output)
        .status()
        .map_err(|e| {
            vec![Diagnostic::err(
                "E0203",
                format!(
                    "cannot invoke {} for LTO section: {} (set OBJCOPY=)",
                    objcopy.display(),
                    e
                ),
                Span::point("", 0, 0),
            )]
        })?;
    let _ = fs::remove_file(&tmp);
    if !status.success() {
        return Err(vec![Diagnostic::err(
            "E0203",
            format!("{} failed to inject .llvm.lto", objcopy.display()),
            Span::point("", 0, 0),
        )]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_yield_e0200() {
        let err = link_relocatable(&[], Path::new("out.o"), false, false)
            .expect_err("empty link must fail");
        assert_eq!(err[0].code, "E0200");
    }
}
