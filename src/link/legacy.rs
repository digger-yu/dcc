// src/link.rs - 旧 host `ld -r` 兼容路径（保留 -r 行为）
//
// 设计边界：
//   - 旧的 -r 模式仍可作为“外发 ld 合并”入口
//   - 新的 -flto 多 TU 链接 / PIE / .so 路径在 src/link/ 模块中
//
// 历史沿用：本文件保持对 host ld + objcopy 的依赖（与早期 .llvm.lto 摘要模式兼容）。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::codegen::shared::elf::global_symbol_names;
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

/// 收集所有输入对象的 STB_GLOBAL / STB_WEAK 符号, 供 `ld -r --gc-sections`
/// 用 `-u <name>` 设为 root, 避免宿主 ld 报"无 root"错误. 重复项
/// 用 HashSet 去重, 输出顺序按"先到先得"保持稳定.
fn collect_global_roots(inputs: &[PathBuf]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut roots: Vec<String> = Vec::new();
    for p in inputs {
        let bytes = match fs::read(p) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for name in global_symbol_names(&bytes) {
            if seen.insert(name.clone()) {
                roots.push(name);
            }
        }
    }
    roots
}

/// 合并多个 ET_REL 目标文件；启用 LTO 时合并 dcc.lto 摘要。
/// 完全使用内置链接器路径，不再回退 host ld -r。
/// 支持 -T script.lds 链接器脚本与 -r 模式组合 (内核模块构建需要)。
pub fn link_relocatable(
    inputs: &[PathBuf],
    output: &Path,
    lto_enabled: bool,
    verbose: bool,
) -> Result<(), Vec<Diagnostic>> {
    link_relocatable_with_script(inputs, output, lto_enabled, verbose, None)
}

/// 合并多个 ET_REL 目标文件，可选指定链接器脚本。
/// 内核模块构建流程: ld -r -T script.lds -o module.o objects...
pub fn link_relocatable_with_script(
    inputs: &[PathBuf],
    output: &Path,
    lto_enabled: bool,
    verbose: bool,
    linker_script: Option<&Path>,
) -> Result<(), Vec<Diagnostic>> {
    if inputs.is_empty() {
        return Err(vec![Diagnostic::err(
            "E0200",
            "no input files for link",
            Span::point("", 0, 0),
        )]);
    }
    // 始终使用内置 relocatable 路径 (不依赖 host ld / objcopy)
    link_relocatable_builtin(inputs, output, lto_enabled, verbose, linker_script)
}

/// 内置 relocatable 路径: 完全自研，不依赖 host ld / objcopy
/// 支持可选链接器脚本 (-T script.lds + -r 组合)
fn link_relocatable_builtin(
    inputs: &[PathBuf],
    output: &Path,
    lto_enabled: bool,
    verbose: bool,
    linker_script: Option<&Path>,
) -> Result<(), Vec<Diagnostic>> {
    let cfg = super::LinkConfig {
        output_kind: super::OutputKind::Relocatable,
        lto: lto_enabled,
        verbose,
        // 内置路径不需要 entry / gc_sections
        gc_sections: false,
        entry: String::new(),
        linker_script: linker_script.map(PathBuf::from),
        ..Default::default()
    };
    match super::link_multi_tu(inputs, output, &cfg) {
        Ok(bytes) => {
            std::fs::write(output, &bytes).map_err(|e| {
                vec![Diagnostic::err(
                    "E0201",
                    format!("cannot write {}: {}", output.display(), e),
                    Span::point(&output.display().to_string(), 0, 0),
                )]
            })?;
            if verbose {
                eprintln!(
                    "dcc-link: builtin -r link {} -> {} ({} bytes)",
                    inputs.len(),
                    output.display(),
                    bytes.len()
                );
            }
            Ok(())
        },
        Err(e) => Err(e),
    }
}

/// 旧 host `ld -r` + `objcopy` 路径 (保留作为回退)
fn link_relocatable_legacy(
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
            .filter_map(|obj| {
                lto_bitcode::deserialize_module(
                    &crate::codegen::shared::elf::read_section_data(obj, ".dcc.lto")
                        .unwrap_or_default(),
                )
                .ok()
            })
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
    // `-r` 模式本意是"保符号", 宿主 ld 在 `--gc-sections` 下会强制要求
    // entry/root, 这里把每个输入的全局符号都加为 -u root, 与 GCC
    // `-r -flto` 行为一致 (保留全部全局符号, 让 LTO/DCE 自己处理
    // 死代码). LTO 期间把 dcc.lto 节以字节形式重写注入, 与是否 GC
    // 无关.
    if lto_enabled {
        ld.arg("--gc-sections");
        for root in collect_global_roots(inputs) {
            ld.arg("-u").arg(root);
        }
    }
    for input in inputs {
        ld.arg(input);
    }
    let ld_path = tool_path("ld", "LD");
    let status = ld.status().map_err(|e| {
        vec![Diagnostic::err(
            "E0202",
            format!("cannot invoke {}: {} (set LD= or install binutils)", ld_path.display(), e),
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

    #[test]
    fn builtin_r_always_used_regardless_of_lto() {
        // 验证 link_relocatable 始终使用内置路径, 不因 LTO 回退 host ld
        use crate::codegen::shared::elf;
        let dir = std::env::temp_dir();
        let text = &[0xc3u8]; // ret
        let sym1 = elf::ElfSym {
            name: "foo_a".into(),
            section: 1,
            value: 0,
            size: 1,
            bind: elf::STB_GLOBAL,
            ty: elf::STT_FUNC,
            visibility: elf::STV_DEFAULT,
        };
        let sym2 = elf::ElfSym {
            name: "foo_b".into(),
            section: 1,
            value: 0,
            size: 1,
            bind: elf::STB_GLOBAL,
            ty: elf::STT_FUNC,
            visibility: elf::STV_DEFAULT,
        };
        let obj1 = elf::build_elf_rel_x86_64(
            text,
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &[sym1],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let obj2 = elf::build_elf_rel_x86_64(
            text,
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &[sym2],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let p1 = dir.join("dcc-r-lto-1.o");
        let p2 = dir.join("dcc-r-lto-2.o");
        let out = dir.join("dcc-r-lto-out.o");
        std::fs::write(&p1, &obj1).unwrap();
        std::fs::write(&p2, &obj2).unwrap();
        // LTO enabled: 应该使用内置路径
        let result = link_relocatable(&[p1.clone(), p2.clone()], &out, true, false);
        assert!(result.is_ok(), "builtin -r with LTO: {:?}", result.err());
        let out_bytes = std::fs::read(&out).unwrap();
        assert_eq!(&out_bytes[0..4], b"\x7fELF");
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
        let _ = std::fs::remove_file(&out);
    }

    /// Test: -r + -T script.lds combination works (kernel module build pattern)
    #[test]
    fn relocatable_with_linker_script() {
        use crate::codegen::shared::elf;
        let dir = std::env::temp_dir();
        let text = &[0xc3u8]; // ret
        let sym1 = elf::ElfSym {
            name: "mod_func".into(),
            section: 1,
            value: 0,
            size: 1,
            bind: elf::STB_GLOBAL,
            ty: elf::STT_FUNC,
            visibility: elf::STV_DEFAULT,
        };
        let obj1 = elf::build_elf_rel_x86_64(
            text,
            &[],
            &[],
            0,
            &[],
            0,
            16,
            16,
            16,
            16,
            16,
            &[sym1],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let p1 = dir.join("dcc-r-script-1.o");
        let out = dir.join("dcc-r-script-out.o");
        let script_path = dir.join("dcc-r-test.lds");
        std::fs::write(&p1, &obj1).unwrap();
        // Simple linker script that defines section layout
        std::fs::write(&script_path, "SECTIONS { .text : { *(.text) } }\n").unwrap();
        let result = link_relocatable_with_script(
            std::slice::from_ref(&p1),
            &out,
            false,
            false,
            Some(&script_path),
        );
        assert!(result.is_ok(), "builtin -r with linker script: {:?}", result.err());
        let out_bytes = std::fs::read(&out).unwrap();
        assert_eq!(&out_bytes[0..4], b"\x7fELF");
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&script_path);
    }
}
