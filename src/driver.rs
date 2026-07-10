// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use std::fs;
use std::io::Write;
use crate::cli::Config;
use crate::diagnostics::Diagnostic;
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
    // 读源
    let src = fs::read(&cfg.input)
        .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read {}: {}", cfg.input.display(), e),
            crate::diagnostics::Span::point(&cfg.input.display().to_string(), 0, 0))])?;

    // 预处理
    let file = cfg.input.display().to_string();
    let (preprocessed, pp_diags) = preproc::preprocess(&src, &file, &cfg.include_paths);
    if !pp_diags.is_empty() {
        for d in &pp_diags { d.print(); }
        if pp_diags.iter().any(|d| matches!(d.sev, crate::diagnostics::Severity::Error)) {
            return Err(pp_diags);
        }
    }

    // 解析
    let (prog, diags) = parser::parse(preprocessed.as_bytes(), &file);
    if !diags.is_empty() { return Err(diags); }

    // dump AST?
    if cfg.dump_ast {
        println!("{:#?}", prog);
        return Ok(());
    }

    // codegen
    let target = Target::resolve(&cfg.target);
    let (bytes, entry) = codegen::emit(&prog, &target);
    if cfg.verbose {
        eprintln!("dcc: entry  = 0x{:x}", entry);
        eprintln!("dcc: size   = {} bytes", bytes.len());
    }

    // 写输出
    let out = &cfg.output;
    let mut f = fs::File::create(out)
        .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write {}: {}", out.display(), e),
            crate::diagnostics::Span::point(&out.display().to_string(), 0, 0))])?;
    f.write_all(&bytes).unwrap();
    // chmod +x
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let mut p = f.metadata().unwrap().permissions();
        p.set_mode(0o755);
        f.set_permissions(p).unwrap();
    }
    eprintln!("dcc: wrote {}", out.display());
    Ok(())
}
