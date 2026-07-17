// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use std::fs;
use std::io::{self, Write};
use crate::mmap_io::MappedInput;
use std::process::Command;
use crate::cli::{Config, InputLang};
use crate::diagnostics::{Diagnostic, Severity, enrich_diags};
use crate::target::Target;
use crate::preproc;
use crate::parser;
use crate::codegen;
use crate::ir;
use crate::sanitizer;
use crate::format_check;
use crate::lto;
pub fn run(cfg: Config) -> Result<(), Vec<Diagnostic>> {
    // -r 多 .o 可重定位链接
    if cfg.link_mode {
        let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
        return crate::link::link_relocatable(&cfg.inputs, &cfg.output, lto_enabled, cfg.verbose);
    }

    let input = cfg.inputs.first().ok_or_else(|| {
        vec![Diagnostic::err("E0100", "no input file", crate::diagnostics::Span::point("", 0, 0))]
    })?;

    // .S / -x assembler-with-cpp: 预处理 + 系统汇编器
    if matches!(cfg.input_lang, InputLang::AssemblerWithCpp)
        || input.extension().map(|e| e == "S").unwrap_or(false) {
        return run_assembler_driver(&cfg, input);
    }

    if cfg.verbose {
        eprintln!("dcc: input  = {}", input.display());
        eprintln!("dcc: target = {}", cfg.target.as_str());
        eprintln!("dcc: std    = {}", cfg.std.as_str());
        eprintln!("dcc: -O{}", cfg.opt);
        if cfg.debug_level > 0 { eprintln!("dcc: -g{}", cfg.debug_level); }
        if cfg.lto_enabled { eprintln!("dcc: -flto"); }
    }

    // 读源：mmap（ADR-006）或 stdin
    let (mapped, file_name) = if input.as_os_str() == "-" {
        let mapped = MappedInput::from_stdin(io::stdin())
            .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read stdin: {}", e),
                crate::diagnostics::Span::point("-", 0, 0))])?;
        (mapped, "-".to_string())
    } else {
        let mapped = MappedInput::open(input)
            .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read {}: {}", input.display(), e),
                crate::diagnostics::Span::point(&input.display().to_string(), 0, 0))])?;
        (mapped, input.display().to_string())
    };
    let src = mapped.as_slice();

    // 预处理
    let (preprocessed, mut pp_diags, include_deps) = preproc::preprocess(
        &src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding,
    );
    enrich_diags(&mut pp_diags, &src);
    if !pp_diags.is_empty() {
        for d in &pp_diags { d.print(); }
        if pp_diags.iter().any(|d| matches!(d.sev, crate::diagnostics::Severity::Error)) {
            return Err(pp_diags);
        }
    }

    // -E 模式：只做预处理，输出到 stdout
    if cfg.preprocess_only {
        print!("{}", preprocessed);
        return Ok(());
    }

    // 解析
    let (mut prog, mut diags) = parser::parse_with_opts(
        preprocessed.as_bytes(),
        &file_name,
        parser::ParseOptions { permissive: cfg.permissive },
    );
    drop(preprocessed);
    enrich_diags(&mut diags, &src);
    for d in &diags {
        if matches!(d.sev, Severity::Warning) {
            d.print();
        }
    }
    if diags.iter().any(|d| matches!(d.sev, Severity::Error)) { return Err(diags); }

    for w in format_check::check_program(&prog) {
        w.print();
        diags.push(w);
    }

    // sanitizer 插桩（AST 层，在 IR lower 之前）
    sanitizer::instrument_program(&mut prog, cfg.sanitizers);

    // dump AST?
    if cfg.dump_ast {
        println!("{:#?}", prog);
        return Ok(());
    }

    // 生成依赖文件 (.d)
    if cfg.gen_dep {
        let dep_path = cfg.dep_file.as_ref().unwrap_or_else(|| &cfg.output);
        let target = cfg.dep_target.as_deref().unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
        let deps: Vec<String> = if cfg.dep_mmd_only {
            filter_user_deps(include_deps, &cfg.system_paths)
        } else {
            include_deps
        };
        let mut dep_out: Box<dyn Write> = if dep_path.to_str() == Some("-") {
            Box::new(io::stdout())
        } else {
            if let Some(parent) = dep_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            Box::new(fs::File::create(dep_path)
                .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write {}: {}", dep_path.display(), e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?)
        };
        writeln!(dep_out, "{}: {}", target, input.display())
            .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write dep file: {}", e),
                crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?;
        for dep in &deps {
            writeln!(dep_out, " {}", dep)
                .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write dep file: {}", e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?;
        }
        if cfg.dep_phony {
            for dep in &deps {
                writeln!(dep_out, "\n{}:", dep)
                    .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write dep file: {}", e),
                        crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?;
            }
        }
    }

    // AST -> typed IR -> validation
    let mut module = ir::lower(prog)?;
    ir::validate(&module)?;
    crate::opt::optimize(&mut module, cfg.opt);
    if cfg.opt > 0 {
        ir::resync(&mut module)?;
        ir::validate(&module)?;
    }

    if cfg.syntax_only {
        return Ok(());
    }

    // codegen
    let mut target = Target::resolve(&cfg.target);
    target.apply_codegen_options(cfg.red_zone, cfg.frame_pointer, cfg.sse);
    if !target.sse {
        let uses_sse = module.functions.iter().any(|f| {
            format!("{:?}", f).contains("Float") || format!("{:?}", f).contains("Double")
        }) || module.globals.iter().any(|g| {
            format!("{:?}", g).contains("Float") || format!("{:?}", g).contains("Double")
        });
        if uses_sse {
            return Err(vec![Diagnostic::err(
                "E0300",
                "floating-point code requires SSE on the SysV AMD64 target; remove -mno-sse",
                crate::diagnostics::Span::point(&file_name, 1, 1),
            )]);
        }
    }

    let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
    let function_sections = cfg.function_sections || lto_enabled;
    let bytes = codegen::emit(
        &module, &target, cfg.relocatable, cfg.emit_asm, &file_name,
        cfg.debug_level, cfg.sanitizers, lto_enabled, function_sections,
        cfg.parallel_codegen,
    );
    if cfg.verbose {
        eprintln!("dcc: size   = {} bytes", bytes.len());
    }

    let out = &cfg.output;
    let mut f = fs::File::create(out)
        .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write {}: {}", out.display(), e),
            crate::diagnostics::Span::point(&out.display().to_string(), 0, 0))])?;
    f.write_all(&bytes).unwrap();
    if !cfg.relocatable && !cfg.emit_asm {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = f.metadata().unwrap().permissions();
            p.set_mode(0o755);
            f.set_permissions(p).unwrap();
        }
    }
    eprintln!("dcc: wrote {}", out.display());
    Ok(())
}

/// 过滤 -MMD 模式下的系统头文件依赖。
fn filter_user_deps(deps: Vec<String>, system_paths: &[std::path::PathBuf]) -> Vec<String> {
    deps.into_iter()
        .filter(|dep| {
            let path = std::path::Path::new(dep);
            !system_paths.iter().any(|sys| path.starts_with(sys))
                && !dep.starts_with("/usr/include")
                && !dep.starts_with("/usr/lib/gcc")
        })
        .collect()
}

/// 独立汇编驱动: 预处理 .S 后调用系统 `as`。
fn run_assembler_driver(cfg: &Config, input: &std::path::Path) -> Result<(), Vec<Diagnostic>> {
    let file_name = input.display().to_string();
    let src = fs::read(input)
        .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read {}: {}", file_name, e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;

    let (preprocessed, mut pp_diags, _) = preproc::preprocess(
        &src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding,
    );
    enrich_diags(&mut pp_diags, &src);
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        for d in &pp_diags { d.print(); }
        return Err(pp_diags);
    }

    if cfg.preprocess_only {
        print!("{}", preprocessed);
        return Ok(());
    }

    let tmp_dir = std::env::temp_dir();
    let asm_path = tmp_dir.join(format!("dcc-asm-{}.s", std::process::id()));
    fs::write(&asm_path, preprocessed.as_bytes())
        .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write temp asm: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;

    let mut cmd = Command::new("as");
    for opt in &cfg.as_options {
        for part in opt.split(',') {
            if !part.is_empty() {
                cmd.arg(part);
            }
        }
    }
    cmd.arg("-o").arg(&cfg.output).arg(&asm_path);
    let status = cmd.status()
        .map_err(|e| vec![Diagnostic::err("E0102", format!("cannot invoke assembler: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;
    let _ = fs::remove_file(&asm_path);
    if !status.success() {
        return Err(vec![Diagnostic::err("E0102", "assembler failed",
            crate::diagnostics::Span::point(&file_name, 0, 0))]);
    }
    eprintln!("dcc: wrote {}", cfg.output.display());
    Ok(())
}
