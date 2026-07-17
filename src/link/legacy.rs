// src/link.rs - 旧 host `ld -r` 兼容路径（保留 -r 行为）
//
// 设计边界：
//   - 旧的 -r 模式仍可作为“外发 ld 合并”入口
//   - 新的 -flto 多 TU 链接 / PIE / .so 路径在 src/link/ 模块中
//
// 历史沿用：本文件保持对 host ld + objcopy 的依赖（与早期 .llvm.lto 摘要模式兼容）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diagnostics::{Diagnostic, Span};
use crate::link::lto_bitcode;

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

/// 合并多个 ET_REL 目标文件；启用 LTO 时合并 dcc.lto 摘要。
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

    let mut bc_modules: Vec<lto_bitcode::BitcodeModule> = if lto_enabled {
        refs.iter()
            .filter_map(|obj| lto_bitcode::deserialize_module(&crate::codegen::elf::read_section_data(obj, ".dcc.lto").unwrap_or_default()).ok())
            .collect()
    } else {
        Vec::new()
    };
    if lto_enabled && !bc_modules.is_empty() {
        lto_bitcode::run_cross_tu_lto(&mut bc_modules);
    }

    if verbose {
        eprintln!(
            "dcc: legacy -r link {} -> {} (ld={}, objcopy={})",
            inputs.len(),
            output.display(),
            tool_path("ld", "LD").display(),
            tool_path("objcopy", "OBJCOPY").display(),
        );
        if lto_enabled {
            eprintln!("dcc: LTO bitcode modules: {}", bc_modules.len());
        }
    }

    let mut ld = tool_command("ld", "LD");
    ld.arg("-r").arg("-o").arg(output);
    if lto_enabled {
        ld.arg("--gc-sections");
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

    if lto_enabled && !bc_modules.is_empty() {
        // 把合并后的位码重新注入 .dcc.lto
        let mut merged = Vec::new();
        for m in &bc_modules {
            let payload = lto_bitcode::serialize_module_from_bc(m);
            merged.extend_from_slice(&payload);
        }
        inject_lto_section(output, &merged)?;
    }

    Ok(())
}

/// 用 objcopy 向 ET_REL 输出追加 .dcc.lto 节。
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
        .arg(format!(".dcc.lto={}", tmp_str))
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
            format!("{} failed to inject .dcc.lto", objcopy.display()),
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
