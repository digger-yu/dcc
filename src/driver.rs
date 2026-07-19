// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use std::fs;
use std::io::{self, Write};
use crate::mmap_io::MappedInput;
use std::process::Command;
use crate::cli::{Config, InputLang, LinkerOutputKind};
use crate::diagnostics::{Diagnostic, Severity, enrich_diags};
use crate::target::Target;
use crate::preproc;
use crate::parser;
use crate::codegen;
use crate::ir;
use crate::link;
use crate::sanitizer;
use crate::format_check;
use crate::lto;
pub fn run(cfg: Config) -> Result<(), Vec<Diagnostic>> {
    // -r 多 .o 可重定位链接（host ld -r 兼容路径）
    if cfg.link_mode && !cfg.built_link {
        let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
        return crate::link::link_relocatable(&cfg.inputs, &cfg.output, lto_enabled, cfg.verbose);
    }

    // -pie / -shared / -fbuiltin-linker：走内置全链接器
    if cfg.built_link && cfg.link_mode {
        return run_builtin_linker(&cfg);
    }

    let input = cfg.inputs.first().ok_or_else(|| {
        vec![Diagnostic::err("E0100", "no input file", crate::diagnostics::Span::point("", 0, 0))]
    })?;

    // .S / -x assembler-with-cpp: 预处理 + 系统汇编器
    if matches!(cfg.input_lang, InputLang::AssemblerWithCpp)
        || input.extension().map(|e| e == "S").unwrap_or(false) {
        return run_assembler_driver(&cfg, input);
    }

    // C++ 前端: -x c++ / .cpp / .cc / .cxx / .CPP / .CXX
    let is_cpp = matches!(cfg.input_lang, InputLang::Cpp)
        || input.extension().and_then(|e| e.to_str())
            .map(|e| matches!(e, "cpp" | "cc" | "cxx" | "CPP" | "CXX"))
            .unwrap_or(false);
    if is_cpp {
        return run_cpp_driver(&cfg, input);
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

/// C++ 编译驱动: M9 接入点
///
/// 流程:
///   1. 预处理 C++ 源 (复用 C 预处理器; C++ 模式不处理 #include <iostream> 等)
///   2. 调 cpp 前端 lex/parse/lower → 降级到 C 字符串
///   3. 写到临时 .c 文件
///   4. 调 C 链路 (re-preprocess / parse / IR / codegen) 写 .o 或 .s
///
/// 设计: C++ 不另起 IR, 而是降级到 C 后复用整条 C 链路 (M7 决策)。
/// 这意味着 C++ 的所有限制 = 现有 C 后端能表达的限制。
fn run_cpp_driver(cfg: &Config, input: &std::path::Path) -> Result<(), Vec<Diagnostic>> {
    use crate::cpp::lower::Lower;
    use crate::cpp::parser::CppParser;

    let file_name = input.display().to_string();

    // -E 模式: 复用 C 预处理器 (C++ 注释 + 基本宏支持, 不解析 #include <iostream> 等)
    if cfg.preprocess_only {
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
        print!("{}", preprocessed);
        return Ok(());
    }

    if cfg.verbose {
        eprintln!("dcc: cpp     = {}", input.display());
        eprintln!("dcc: target  = {}", cfg.target.as_str());
        eprintln!("dcc: std     = {}", cfg.std.as_str());
        eprintln!("dcc: -O{}", cfg.opt);
    }

    // 1. 预处理 (复用 C 预处理器, 暂不解析 C++ 标准库头)
    let src = fs::read(input)
        .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read {}: {}", file_name, e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;
    let (preprocessed, mut pp_diags, _include_deps) = preproc::preprocess(
        &src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding,
    );
    enrich_diags(&mut pp_diags, &src);
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        for d in &pp_diags { d.print(); }
        return Err(pp_diags);
    }

    // 2. C++ lex + parse + lower
    let mut parser = CppParser::new(preprocessed.as_bytes(), &file_name);
    let decls = parser.parse_top_level()
        .map_err(|e| vec![e.to_diag(&file_name)])?;
    let mut lower = Lower::new();
    lower.lower_decls(&decls);
    let c_src = lower.finish();

    // -dump-ast: 打印 C++ AST 后退出
    if cfg.dump_ast {
        println!("{:#?}", decls);
        return Ok(());
    }

    // -fsyntax-only: 只跑 lower, 不调 dcc 编译
    if cfg.syntax_only {
        return Ok(());
    }

    // 3. 写临时 .c
    let tmp = if let Ok(p) = std::env::var("DCC_CPP_KEEP_TMP") {
        std::path::PathBuf::from(p)
    } else {
        std::env::temp_dir().join(format!(
            "dcc-cpp-{}-{}.c",
            std::process::id(),
            input.file_name().and_then(|n| n.to_str()).unwrap_or("input")
        ))
    };
    fs::write(&tmp, c_src)
        .map_err(|e| vec![Diagnostic::err("E0101", format!("cannot write tmp: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;

    // 4. 调 dcc C 链路编译 (走相同 dcc 二进制, 复用 codegen/IR)
    let dcc_bin = std::env::current_exe()
        .map_err(|e| vec![Diagnostic::err("E0102", format!("cannot find dcc: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;
    let mut cmd = Command::new(&dcc_bin);
    cmd.arg("-c");
    cmd.arg(format!("-O{}", cfg.opt));
    cmd.arg(format!("-std={}", cfg.std.as_str()));
    for inc in &cfg.include_paths { cmd.arg("-I").arg(inc); }
    for sys in &cfg.system_paths { cmd.arg("-isystem").arg(sys); }
    for d in &cfg.defines {
        if let Some(v) = &d.1 { cmd.arg("-D").arg(format!("{}={}", d.0, v)); }
        else { cmd.arg("-D").arg(&d.0); }
    }
    if cfg.debug_level > 0 { cmd.arg(format!("-g{}", cfg.debug_level)); }
    if cfg.emit_asm { cmd.arg("-S"); }
    cmd.arg("-o").arg(&cfg.output);
    cmd.arg(&tmp);

    if cfg.verbose {
        eprintln!("dcc: dcc -c -O{} -o {} {}", cfg.opt, cfg.output.display(), tmp.display());
    }

    let out = cmd.output()
        .map_err(|e| vec![Diagnostic::err("E0102", format!("cannot spawn dcc: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;
    if std::env::var("DCC_CPP_KEEP_TMP").is_err() {
        let _ = fs::remove_file(&tmp);
    }
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(vec![Diagnostic::err("E0301",
            format!("dcc C backend failed for {}: {}", file_name, stderr),
            crate::diagnostics::Span::point(&file_name, 0, 0))]);
    }
    eprintln!("dcc: wrote {}", cfg.output.display());
    Ok(())
}

/// 内置全链接器：合并多个 .o → 写 ET_EXEC / ET_DYN（PIE / .so）
fn run_builtin_linker(cfg: &Config) -> Result<(), Vec<Diagnostic>> {
    if cfg.inputs.is_empty() {
        return Err(vec![Diagnostic::err(
            "E0300",
            "no input files for built-in linker",
            crate::diagnostics::Span::point("", 0, 0),
        )]);
    }
    let output_kind = match cfg.output_kind {
        LinkerOutputKind::Executable => link::OutputKind::Executable,
        LinkerOutputKind::PIE => link::OutputKind::PIE,
        LinkerOutputKind::Shared => link::OutputKind::Shared,
    };
    let mut link_cfg = link::LinkConfig {
        output_kind,
        lto: cfg.linker_lto,
        entry: cfg.linker_entry.clone().unwrap_or_else(|| {
            if matches!(output_kind, link::OutputKind::PIE) { "_start".into() }
            else if matches!(output_kind, link::OutputKind::Shared) { String::new() }
            else { "main".into() }
        }),
        base_addr: 0x400000,
        soname: cfg.soname.clone(),
        needed: cfg.link_libs.iter().map(|s| format!("lib{}.so", s)).collect(),
        strip_debug: false,
        gc_sections: true,
        allowed_undef: vec![
            "memcpy".into(), "memmove".into(), "memset".into(), "memcmp".into(),
            "printf".into(), "puts".into(), "exit".into(), "abort".into(),
            "malloc".into(), "free".into(),
            "__stack_chk_fail".into(), "__cxa_atexit".into(), "__cxa_finalize".into(),
            "_start".into(), "main".into(),
        ],
        exclude_libs: cfg.exclude_libs.clone(),
        build_id: cfg.build_id,
        no_undefined_version: cfg.no_undefined_version,
        verbose: cfg.verbose,
        opts: cfg.link_opts.clone(),
        libs: cfg.link_libs.clone(),
        gc_sections_root: cfg.gc_sections_root.clone(),
        no_semantic_interposition: cfg.no_semantic_interposition,
        fuse_ld: cfg.fuse_ld.clone(),
        version_table: link::symver::VersionTable::new(),
        target_clones: link::target_clones::TargetClonesTable::new(),
        fini_builder: link::fini_dispatch::FiniArrayBuilder::new(),
    };
    // 解析 .symver 指令: 从 raw_args 提取 __asm__(".symver ...") 模式
    // 实际项目中由 codegen 阶段把 .symver 写入 .symver 节, 此处给个兜底:
    for arg in &cfg.raw_args {
        if let Some(rest) = arg.strip_prefix(".symver=") {
            link_cfg.version_table.add_symver(&format!(".symver {}", rest));
        }
    }
    if cfg.linker_entry.is_some() {
        link_cfg.entry = cfg.linker_entry.clone().unwrap();
    }
    let bytes = if cfg.linker_lto {
        // LTO 路径：暂时不读取 .dcc.lto（保留给后续：load bitcode 后做 cross-TU 优化）
        link::link_multi_tu(&cfg.inputs, &cfg.output, &link_cfg)?
    } else {
        link::link_multi_tu(&cfg.inputs, &cfg.output, &link_cfg)?
    };
    std::fs::write(&cfg.output, &bytes).map_err(|e| {
        vec![Diagnostic::err(
            "E0301",
            format!("cannot write {}: {}", cfg.output.display(), e),
            crate::diagnostics::Span::point(&cfg.output.display().to_string(), 0, 0),
        )]
    })?;
    if !matches!(output_kind, link::OutputKind::Shared) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&cfg.output) {
                let mut p = meta.permissions();
                p.set_mode(0o755);
                let _ = std::fs::set_permissions(&cfg.output, p);
            }
        }
    }
    eprintln!("dcc: built-in linker wrote {} ({} bytes, {:?})",
        cfg.output.display(), bytes.len(), output_kind);
    Ok(())
}
