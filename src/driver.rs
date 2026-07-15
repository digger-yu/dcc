// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use std::fs;
use std::io::{self, Read, Write};
use crate::cli::Config;
use crate::diagnostics::{Diagnostic, Severity, enrich_diags};
use crate::target::Target;
use crate::preproc;
use crate::parser;
use crate::codegen;
use crate::ir;

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
    let (preprocessed, mut pp_diags, include_deps) = preproc::preprocess(&src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes);
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
    // 解析完成后立即释放预处理字符串 — 减少 codegen 阶段的峰值内存
    // 第一性原理: 不再需要的数据应尽快归还给操作系统
    drop(preprocessed);
    enrich_diags(&mut diags, &src);
    // 仅错误级别的诊断终止编译, warnings/notes 不影响代码生成
    if diags.iter().any(|d| matches!(d.sev, Severity::Error)) { return Err(diags); }

    // dump AST?
    if cfg.dump_ast {
        println!("{:#?}", prog);
        return Ok(());
    }

    // 生成依赖文件 (.d) — 在 -fsyntax-only 之前, 因为内核构建系统在语法检查时也需要 .d
    // 第一性原理: 按需生成最小依赖文件即可满足内核构建, 不必追踪每个头文件
    if cfg.gen_dep {
        let dep_path = cfg.dep_file.as_ref().unwrap_or_else(|| &cfg.output);
        let target = cfg.dep_target.as_deref().unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
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
        writeln!(dep_out, "{}: {}", target, cfg.input.display())
            .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write dep file: {}", e),
                crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?;
        for dep in include_deps {
            writeln!(dep_out, " {}", dep)
                .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write dep file: {}", e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0))])?;
        }
    }

    // AST -> typed IR -> validation。即使 -fsyntax-only 也必须完成语义验证。
    let mut module = ir::lower(prog)?;
    ir::validate(&module)?;
    crate::opt::optimize(&mut module, cfg.opt);

    // -fsyntax-only: 验证成功后直接返回, 不生成代码
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
    let bytes = codegen::emit(&module, &target, cfg.relocatable, cfg.emit_asm, &file_name);
    if cfg.verbose {
        eprintln!("dcc: size   = {} bytes", bytes.len());
    }

    // 写输出
    let out = &cfg.output;
    let mut f = fs::File::create(out)
        .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write {}: {}", out.display(), e),
            crate::diagnostics::Span::point(&out.display().to_string(), 0, 0))])?;
    f.write_all(&bytes).unwrap();
    // chmod +x (仅在生成可执行 ELF 时)
    if !cfg.relocatable && !cfg.emit_asm {
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let mut p = f.metadata().unwrap().permissions();
            p.set_mode(0o755);
            f.set_permissions(p).unwrap();
        }
    }
    eprintln!("dcc: wrote {}", out.display());



    Ok(())
}
