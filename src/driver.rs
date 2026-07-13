// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use std::fs;
use std::io::{self, Read, Write};
use crate::cli::Config;
use crate::diagnostics::{Diagnostic, enrich_diags};
use crate::target::Target;
use crate::preproc;
use crate::parser;
use crate::codegen;

pub fn run(cfg: Config) -> Result<(), Vec<Diagnostic>> {
    if cfg.verbose {
        eprintln!("dcc: input  = {}", cfg.input.display());
        eprintln!("dcc: target = {}", cfg.target.as_str());
        eprintln!("dcc: std    = {}", cfg.std.as_str());
        eprintln!("dcc: -O{}", cfg.opt);
    }

    // 读源：支持文件和 stdin（`-`）
    let (src, file_name) = if cfg.input.as_os_str() == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)
            .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read stdin: {}", e),
                crate::diagnostics::Span::point("-", 0, 0))])?;
        (buf, "-".to_string())
    } else {
        let s = fs::read(&cfg.input)
            .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read {}: {}", cfg.input.display(), e),
                crate::diagnostics::Span::point(&cfg.input.display().to_string(), 0, 0))])?;
        (s, cfg.input.display().to_string())
    };

    // 预处理
    let (preprocessed, mut pp_diags) = preproc::preprocess(&src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes);
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
    let (prog, mut diags) = parser::parse(preprocessed.as_bytes(), &file_name);
    enrich_diags(&mut diags, &src);
    if !diags.is_empty() { return Err(diags); }

    // dump AST?
    if cfg.dump_ast {
        println!("{:#?}", prog);
        return Ok(());
    }

    // -fsyntax-only: 解析成功后直接返回, 不生成代码
    if cfg.syntax_only {
        return Ok(());
    }

    // codegen
    let target = Target::resolve(&cfg.target);
    let bytes = codegen::emit(&prog, &target, cfg.relocatable);
    if cfg.verbose {
        eprintln!("dcc: size   = {} bytes", bytes.len());
    }

    // 写输出
    let out = &cfg.output;
    let mut f = fs::File::create(out)
        .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write {}: {}", out.display(), e),
            crate::diagnostics::Span::point(&out.display().to_string(), 0, 0))])?;
    f.write_all(&bytes).unwrap();
    // chmod +x (仅在非 relocatable 模式下)
    if !cfg.relocatable {
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let mut p = f.metadata().unwrap().permissions();
            p.set_mode(0o755);
            f.set_permissions(p).unwrap();
        }
    }
    eprintln!("dcc: wrote {}", out.display());

    // 生成依赖文件 (.d) — 用于内核构建系统的 make 依赖跟踪
    // 第一性原理: 按需生成最小依赖文件即可满足内核构建, 不必追踪每个头文件
    if cfg.gen_dep {
        let dep_path = cfg.dep_file.as_ref().unwrap_or(out);
        let target = cfg.dep_target.as_deref().unwrap_or_else(|| out.to_str().unwrap_or("unknown"));
        let mut dep_out: Box<dyn Write> = if dep_path.to_str() == Some("-") {
            Box::new(io::stdout())
        } else {
            // 确保依赖目录存在
            if let Some(parent) = dep_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            Box::new(fs::File::create(dep_path)
                .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write {}: {}", dep_path.display(), e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?)
        };
        // 写一个最小依赖: 目标只依赖自身 (内核 fixdep 工具需要至少存在这个文件)
        // 格式: target.o: target.c
        writeln!(dep_out, "{}: {}", target, cfg.input.display())
            .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write dep file: {}", e),
                crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?;
    }

    Ok(())
}
