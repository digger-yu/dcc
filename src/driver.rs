// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use crate::mmap_io::MappedInput;
use std::process::Command;
use crate::cli::{Config, InputLang, LinkerOutputKind};
use crate::diagnostics::{
    Diagnostic, DiagnosticsConfig, Severity, enrich_diags,
};
use crate::target::Target;
use crate::preproc;
use crate::parser;
use crate::codegen;
use crate::ir;
use crate::link;
use crate::sanitizer;
use crate::format_check;
use crate::lto;
use crate::profile;

/// 把预处理器阶段产生的非致命诊断打印到 stderr。
/// PCH 不兼容 (W0400) / 缓存写失败 (W0402) 等场景会被转储; 错误会通过
/// 调用方已有的错误检查路径处理 (Err(pp_diags) → main.rs 输出).
/// 设计: 与 parser/sanitizer 警告保持视觉一致, 但不重复 main.rs 的
/// 统一错误输出逻辑 (本函数只针对 warning 级别).
fn emit_preproc_warnings(diags: &[Diagnostic]) {
    for d in diags {
        if !matches!(d.sev, Severity::Error) {
            eprintln!("dcc: {}: {}", d.code, d.msg);
        }
    }
}

/// 根据 Config 构造 DiagnosticsConfig。-v / DCC_FORCE_COLOR 等不参与,
/// 直接由 ColorPolicy::Auto 走 IsTerminal 判定。
fn diag_cfg(cfg: &Config) -> DiagnosticsConfig {
    DiagnosticsConfig {
        color: cfg.diag_color,
        format: cfg.diag_format,
        show_template_tree: cfg.diag_show_template_tree,
    }
}

/// 根据 --sysroot= 重写 cfg 的 include / library 路径, 产出一个新 cfg.
/// 不修改原 cfg; 调用方负责在后续路径使用结果.
///
/// 规则: 若 cfg.sysroot == Some(p), 则:
///   - include_paths 前面加 <p>/include, <p>/usr/include
///   - link_libs 不动 (-L 由 host linker 自动处理)
fn apply_sysroot(mut cfg: Config) -> Config {
    let Some(sr) = cfg.sysroot.clone() else { return cfg; };
    if cfg.verbose {
        eprintln!("dcc: sysroot = {}", sr.display());
    }
    // 前缀: <sysroot>/include, <sysroot>/usr/include
    let mut new_inc = Vec::with_capacity(cfg.include_paths.len() + 2);
    let p1 = sr.join("include");
    if p1.is_dir() || !cfg.include_paths.is_empty() {
        new_inc.push(p1);
    }
    let p2 = sr.join("usr").join("include");
    if p2.is_dir() || !cfg.include_paths.is_empty() {
        new_inc.push(p2);
    }
    new_inc.append(&mut cfg.include_paths);
    cfg.include_paths = new_inc;
    cfg
}

/// 跨数据源权重下发: 从 -fprofile-use / -fauto-profile / -flbr-profile /
/// -fmemory-profile-use 指定的文件读 4 个 profile 源, 聚合为
/// `ProfileWeights`, 写入每个 FuncDecl 的 `is_cold` / `is_hot` 字段.
///
/// 设计: 任意源文件读取失败都 silently 跳过 (回退到 "无权重" 状态),
/// 不阻塞编译, 与 GCC / Clang 的 "profile 不匹配时仍可编译" 行为一致.
fn apply_profile_weights(prog: &mut crate::ast::Program, prof: &profile::ProfileConfig) {
    use crate::ast::Decl;

    // 1) PGO: 读 .profraw
    let pgo_w = prof.pgo_use.as_ref().and_then(|p| {
        profile::pgo::Profraw::read(p).ok()
            .map(|p| profile::pgo::PgoWeights::from_profraw(&p))
    }).unwrap_or_default();

    // 2) AutoFDO
    let autofdo = prof.auto_profile.as_ref()
        .or(prof.sample_use.as_ref())
        .and_then(|p| profile::autofdo::AutoFdoProfile::read(p).ok())
        .unwrap_or_default();

    // 3) LBR
    let lbr = prof.lbr_profile.as_ref()
        .and_then(|p| profile::lbr::LbrProfile::read_perf_brstack(p).ok())
        .unwrap_or_default();

    // 4) MemProf
    let memprof = prof.memprof_use.as_ref()
        .and_then(|p| profile::memprof::MemProfData::read(p).ok())
        .unwrap_or_default();

    let weights = profile::weights::ProfileWeights::from_sources(
        &pgo_w, &autofdo, &lbr, &memprof);

    let hot = weights.hot_funcs();
    let cold = weights.cold_funcs();
    for d in &mut prog.decls {
        if let Decl::Func(f) = d {
            if f.body.is_none() { continue; }
            if hot.contains(&f.name.name) { f.is_hot = true; }
            if cold.contains(&f.name.name) { f.is_cold = true; }
        }
    }
}

pub fn run(cfg: Config) -> Result<(), Vec<Diagnostic>> {
    // --sysroot= 重写 include / library 路径. 必须在最早期调用, 让后续
    // 预处理器 / 链接器都看到正确路径.
    let cfg = apply_sysroot(cfg);

    // -r 多 .o 可重定位链接（host ld -r 兼容路径）
    if cfg.link_mode && !cfg.built_link {
        let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
        return crate::link::link_relocatable(&cfg.inputs, &cfg.output, lto_enabled, cfg.verbose);
    }

    // -pie / -shared / -fbuiltin-linker / .o 自动链接：走内置全链接器
    if cfg.built_link {
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

    // 默认可执行路径：委托 host `cc` 做最终链接。
    // dcc 的 x86_64 codegen 在 executable 模式下遇外部调用（如 `fopen`）会
    // panic "unresolved label: <name>"，因为 dcc 内置链接器不能跨进程
    // 访问 libc 符号表。与 GCC 一致：dcc 只出 .o，host cc 负责链接。
    if needs_host_cc_link(&cfg) {
        return run_with_host_link(cfg);
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
    let pp_out = preproc::preprocess(
        src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding, &cfg.pch_opts,
    );
    let mut pp_diags = pp_out.diags;
    let preprocessed = pp_out.text;
    let pack_history = pp_out.pack_history;
    let weak_symbols = pp_out.weak_symbols;
    let include_deps = pp_out.deps;
    let pp_macros = pp_out.macros;
    enrich_diags(&mut pp_diags, src);
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        return Err(pp_diags);
    }
    // 把预处理器阶段的非致命诊断 (PCH 不兼容 / 缓存跳过等) 打印到 stderr.
    // 这与 -E 模式共享同一路径, 即使 -E 也会显示 PCH 警告.
    emit_preproc_warnings(&pp_diags);

    // -E 模式：只做预处理，输出到 stdout
    if cfg.preprocess_only {
        print!("{}", preprocessed);
        // -dM: 额外输出所有宏定义 (供 kernel 探测编译器用)
        if cfg.dump_macros {
            // 按宏名字典序输出，便于差异比较
            let mut sorted = pp_macros;
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, params, variadic, body) in &sorted {
                match params {
                    None => println!("#define {} {}", name, body),
                    Some(p) => {
                        let p_str = if *variadic {
                            let (last, rest) = p.split_last().unwrap();
                            if rest.is_empty() {
                                format!("{}, ...", last)
                            } else {
                                format!("{}, {}, ...", rest.join(", "), last)
                            }
                        } else {
                            p.join(", ")
                        };
                        println!("#define {}({}) {}", name, p_str, body);
                    }
                }
            }
        }
        return Ok(());
    }

    // 解析
    let (mut prog, mut diags) = parser::parse_with_opts(
        preprocessed.as_bytes(),
        &file_name,
        parser::ParseOptions { permissive: cfg.permissive, pack_history: pack_history.clone(), weak_symbols: weak_symbols.clone(), warn_int_truncation: cfg.warn_int_truncation, warn_int_conversion: cfg.warn_int_conversion, is_c23: matches!(cfg.std, crate::cli::StdMode::C23 | crate::cli::StdMode::Gnu23) },
    );
    drop(preprocessed);
    drop(pack_history);
    drop(weak_symbols);
    let dcfg = diag_cfg(&cfg);
    enrich_diags(&mut diags, src);
    // 打印 parser 警告(注意: parser 错误会在下面 return Err,
    // 由 main.rs 统一输出,避免重复)
    for d in &diags {
        if matches!(d.sev, Severity::Warning) {
            d.print_with(&dcfg);
        }
    }
    if diags.iter().any(|d| matches!(d.sev, Severity::Error)) { return Err(diags); }

    let wlist: Vec<Diagnostic> = format_check::check_program(&prog);
    for w in &wlist {
        w.print_with(&dcfg);
        diags.push(w.clone());
    }
    drop(wlist);

    // sanitizer 插桩（AST 层，在 IR lower 之前）
    sanitizer::instrument_program(&mut prog, cfg.sanitizers);

    // PGO 插桩（AST 层）: -fprofile-generate 时在每个函数体顶部插入
    // `__dcc_pgo_count_fn(idx)` 调用. 运行期由用户提供的 runtime helper
    // (或 dcc 内置桩) 累加 idx -> counter, atexit 写 `<name>.profraw`.
    // 权重读回 (`-fprofile-use`) 仅解析 .profraw, 不修改 codegen.
    if cfg.profile.pgo_generate.is_some() {
        let pgo_cfg = profile::pgo::PgoConfig {
            generate: true,
            use_file: cfg.profile.pgo_use.as_ref().map(|p| p.display().to_string()),
            out_dir: cfg.profile.pgo_generate.as_ref().map(|p| p.display().to_string()),
        };
        profile::pgo::instrument_program(&mut prog, &pgo_cfg);
    }

    // 跨数据源权重下发: 加载 4 个 profile 源 (PGO/AutoFDO/LBR/MemProf),
    // 聚合为 ProfileWeights, 写入每个 FuncDecl 的 is_cold / is_hot 字段,
    // 让 codegen 把 hot 函数放到 .text.hot, cold 函数放到 .text.unlikely.
    // 任意 -fprofile-use / -fauto-profile / -flbr-profile / -fmemory-profile-use
    // 任一被设置才执行; 缺省保持 noop, 不影响默认编译.
    if cfg.profile.any_use() {
        apply_profile_weights(&mut prog, &cfg.profile);
    }

    // dump AST?
    if cfg.dump_ast {
        println!("{:#?}", prog);
        return Ok(());
    }

    // 生成依赖文件 (.d)
    if cfg.gen_dep {
        let dep_path = cfg.dep_file.as_ref().unwrap_or(&cfg.output);
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
    target.set_stack_protector(cfg.stack_protector);
    if !target.sse {
        let uses_sse = module.functions.iter().any(|f| {
            format!("{:?}", f).contains("Float") || format!("{:?}", f).contains("Double")
        }) || module.globals.iter().any(|g| {
            format!("{:?}", g).contains("Float") || format!("{:?}", g).contains("Double")
        });
        if uses_sse {
            let mut diags = vec![Diagnostic::err(
                "E0300",
                "floating-point code requires SSE on the SysV AMD64 target; remove -mno-sse",
                crate::diagnostics::Span::point(&file_name, 1, 1),
            )];
            enrich_diags(&mut diags, src);
            return Err(diags);
        }
    }

    let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
    let function_sections = cfg.function_sections || lto_enabled;
    let bytes = codegen::emit(
        &module, &target, cfg.relocatable, cfg.emit_asm, &file_name,
        cfg.debug_level, cfg.sanitizers, lto_enabled, function_sections,
        cfg.parallel_codegen, cfg.bounds_check, cfg.trapv,
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

    // 自举回归对比: -c/--bootstrap-compare=<ref.o> 模式.
    // 编译结束后, 把当前 .o 与 ref.o 做三层对比 (byte / struct /
    // semantic). 仅做诊断打印, 不阻塞编译, 与 GCC -fcompare-debug 行为一致.
    if let Some(ref_path) = cfg.bootstrap_compare.as_ref() {
        match crate::bootstrap::compare_files(ref_path, out) {
            Ok(report) => {
                eprintln!("dcc: bootstrap compare vs {}:", ref_path.display());
                eprint!("{}", report.format());
            }
            Err(e) => {
                eprintln!("dcc: bootstrap compare error: {e}");
            }
        }
    }

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

    let pp_out = preproc::preprocess(
        &src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding, &cfg.pch_opts,
    );
    let mut pp_diags = pp_out.diags;
    let preprocessed = pp_out.text;
    enrich_diags(&mut pp_diags, &src);
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
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
    let output = cmd.output()
        .map_err(|e| vec![Diagnostic::err("E0102", format!("cannot invoke assembler: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;
    let _ = fs::remove_file(&asm_path);
    if !output.status.success() {
        // 把汇编器自身 stderr / stdout 折叠进 message, 避免与 snapshot 漂移
        let mut msg = String::from("assembler failed");
        let stderr_s = String::from_utf8_lossy(&output.stderr);
        let stdout_s = String::from_utf8_lossy(&output.stdout);
        for chunk in [stderr_s.trim_end(), stdout_s.trim_end()] {
            if !chunk.is_empty() {
                msg.push_str(": ");
                msg.push_str(chunk);
            }
        }
        return Err(vec![Diagnostic::err("E0102", msg,
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
        let pp_out = preproc::preprocess(
            &src, &file_name, &cfg.include_paths, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding, &cfg.pch_opts,
        );
        let mut pp_diags = pp_out.diags;
        enrich_diags(&mut pp_diags, &src);
        if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
            return Err(pp_diags);
        }
        print!("{}", pp_out.text);
        return Ok(());
    }

    if cfg.verbose {
        eprintln!("dcc: cpp     = {}", input.display());
        eprintln!("dcc: target  = {}", cfg.target.as_str());
        eprintln!("dcc: std     = {}", cfg.std.as_str());
        eprintln!("dcc: -O{}", cfg.opt);
    }

    // 1. 预处理 (复用 C 预处理器, 默认追加 stdlib headers 路径)
    let src = fs::read(input)
        .map_err(|e| vec![Diagnostic::err("E0100", format!("cannot read {}: {}", file_name, e),
            crate::diagnostics::Span::point(&file_name, 0, 0))])?;
    let mut cpp_includes: Vec<PathBuf> = cfg.include_paths.iter()
        .map(PathBuf::from).collect();
    if let Some(stdlib_inc) = crate::cpp::driver::default_stdlib_include() {
        cpp_includes.push(stdlib_inc);
    }
    let pp_out = preproc::preprocess(
        &src, &file_name, &cpp_includes, &cfg.system_paths, &cfg.defines, cfg.std, &cfg.force_includes, cfg.freestanding, &cfg.pch_opts,
    );
    let mut pp_diags = pp_out.diags;
    enrich_diags(&mut pp_diags, &src);
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        return Err(pp_diags);
    }

    // 2. C++ lex + parse + lower
    let mut parser = CppParser::new(pp_out.text.as_bytes(), &file_name);
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
        arch: crate::debug::ArchKind::X86_64,
        dynamic_linker: None, // TODO: parse from -Wl,--dynamic-linker=
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

/// 判别是否走 “dcc → host cc” 双阶段路径。
///
/// 默认的可执行输出 (`dcc foo.c`) 走 dcc 内置链接器；后者生成 ET_EXEC，
/// 在 dcc 的 x86_64 后端中遇到 libc 调用（如 `fopen` / `printf`）会因
/// 符号未定义而 panic "unresolved label: <name>"。本谓词判断是否满足
/// “dcc 只出 .o，host cc 负责最终链接” 的前置条件：
///   1. 单一输入
///   2. 既不是 -c / -S / -E / -fsyntax-only / -r / -fbuiltin-linker
///   3. 输出文件是 “可执行”（不是 .o / .s / .a / .so）
///
/// 一旦命中，run() 会交递到 `run_with_host_link`。
fn needs_host_cc_link(cfg: &Config) -> bool {
    if cfg.link_mode { return false; }          // -r
    if cfg.built_link { return false; }         // -pie / -shared / -fbuiltin-linker
    if cfg.relocatable { return false; }        // -c
    if cfg.emit_asm { return false; }           // -S
    if cfg.preprocess_only { return false; }    // -E
    if cfg.syntax_only { return false; }        // -fsyntax-only
    if cfg.dump_ast { return false; }           // -dump-ast
    if cfg.inputs.len() != 1 { return false; }
    let out = cfg.output.to_string_lossy();
    if out.ends_with(".o") || out.ends_with(".s")
        || out.ends_with(".a") || out.ends_with(".so")
    {
        return false;
    }
    true
}

/// 委托 host `cc` 做最终链接：dcc 先以 -c 产生临时 .o，再调 cc 链接。
///
/// 与 GCC 默认行为一致：dcc 只负责 .c → .o，host 链接器负责 .o + libc
/// → 可执行。这使 dcc 能在不动 ET_EXEC codegen 的前提下，调过
/// “unresolved label: libc 符号” 的 panic，同时让 host cc 自动加上
/// crt0 / libc / crtend 运行时起步代码。
fn run_with_host_link(mut cfg: Config) -> Result<(), Vec<Diagnostic>> {
    let final_output = cfg.output.clone();
    let link_libs = cfg.link_libs.clone();
    let sysroot = cfg.sysroot.clone();
    let verbose = cfg.verbose;
    let input_name = cfg.inputs.first()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    let tmp_o = std::env::temp_dir().join(format!(
        "dcc-hostlink-{}-{}.o",
        std::process::id(),
        input_name,
    ));
    if verbose {
        eprintln!("dcc: host-link: {} -> {} -> {}",
            cfg.inputs[0].display(), tmp_o.display(), final_output.display());
    }
    // 1) 临时 .o：设 relocatable=true 后递归 run()
    cfg.output = tmp_o.clone();
    cfg.relocatable = true;
    // 强制走内置链接器 ≠ 1 路径是上面“递归后”需走的；现在 cfg.built_link
    // 仍是 false，所以 .o 由 run() 末端 codegen 直接输出。
    let result = run(cfg);
    if result.is_err() {
        let _ = fs::remove_file(&tmp_o);
        return result;
    }
    // 2) 调 host cc 链接
    let link_result = link_with_host_cc(
        &tmp_o, &final_output, &link_libs,
        sysroot.as_deref(), verbose,
    );
    let _ = fs::remove_file(&tmp_o);
    if link_result.is_ok() {
        eprintln!("dcc: {} (via host cc)", final_output.display());
    }
    link_result
}

/// 调 host `cc` 把单 .o 链成可执行。传递 --sysroot= 与 -l 列表。
/// 失败时返回包含 cc 自身 stdout/stderr 的诊断 E0202。
///
/// 启动选项 (与 dcc 旧内部链接器输出对齐):
///   - -no-pie : dcc 现有 codegen 采用 RIP-相对 / 静态地址访问 TLS / 全局;
///               PIE 下需要重写为 -fpie. 在 -no-pie 下生成 ET_EXEC, 所有地址
///               运行时固定, 与 dcc codegen 期望一致.
///   - -z norelro : 避免 GNU_RELRO 覆盖 .tdata, 造成 TLS 写入段错误 (因为
///               dcc 的 .tdata 位于 .data LOAD 段首, 与 RELRO 重叠).
///   - -Wl,-z,noexecstack : dcc 当前不输出 .note.GNU-stack, 避免 ld 警告
///               "missing .note.GNU-stack section implies executable stack".
fn link_with_host_cc(
    obj_path: &Path,
    output: &Path,
    link_libs: &[String],
    sysroot: Option<&Path>,
    verbose: bool,
) -> Result<(), Vec<Diagnostic>> {
    // CC 环境变量优先, 与 GNU make 一致; 缺省为 cc.
    // 防递归: GCC 构建中 `CC=dcc` 是常见写法, 如果这里读 CC=dcc,
    // 会导致 dcc 递归调用 dcc, 最终试图把 .o 当 C 源预处,
    // 触发 E0208 ("source is not valid UTF-8"). 遇 CC 指向自己时回退
    // 到默认 "cc" (host 系统上 99% 是 gcc), 与 gcc 手工调用 dcc 时
    // 设 `CC=gcc` 一致.
    let cc = match std::env::var("CC") {
        Ok(v) if v != std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_default() => v,
        _ => "cc".to_string(),
    };
    let mut cmd = Command::new(&cc);
    cmd.arg(obj_path).arg("-o").arg(output);
    cmd.arg("-no-pie");
    cmd.arg("-z").arg("norelro");
    cmd.arg("-Wl,-z,noexecstack");
    if let Some(sr) = sysroot {
        cmd.arg(format!("--sysroot={}", sr.display()));
    }
    for lib in link_libs {
        cmd.arg(format!("-l{}", lib));
    }
    if verbose {
        eprintln!("dcc: invoking {}: {:?}", cc, cmd);
    }
    let out = cmd.output().map_err(|e| {
        vec![Diagnostic::err("E0202",
            format!("cannot invoke {}: {} (set CC= or install gcc)", cc, e),
            crate::diagnostics::Span::point("", 0, 0))]
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut msg = format!("{} link failed (exit {:?})",
            cc, out.status.code());
        for chunk in [stderr.trim_end(), stdout.trim_end()] {
            if !chunk.is_empty() {
                msg.push_str(": ");
                msg.push_str(chunk);
            }
        }
        return Err(vec![Diagnostic::err("E0202", msg,
            crate::diagnostics::Span::point(&output.display().to_string(), 0, 0))]);
    }
    Ok(())
}
