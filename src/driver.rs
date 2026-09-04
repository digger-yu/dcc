// src/driver.rs - 驱动: 读文件 -> 预处理 -> 解析 -> codegen -> 写文件
// 第一性原理: 编译器是"输入/输出"的胶水;所有业务在 preproc/parse/codegen 内部完成。

use crate::cli::{Config, InputLang, LinkerOutputKind};
use crate::codegen;
use crate::diagnostics::{enrich_diags, Diagnostic, DiagnosticsConfig, Severity};
use crate::format_check;
use crate::incremental::{self, CacheResult, IncrementalCache};
use crate::ir;
use crate::link;
use crate::lto;
use crate::mmap_io::MappedInput;
use crate::parser;
use crate::preproc;
use crate::preproc::LineSpan;
use crate::profile;
use crate::sanitizer;
use crate::target::Target;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 判断 IR 类型是否(递归)包含浮点成分, 供 -mno-sse 下的 E0300 检测使用
fn ir_type_uses_float(t: &ir::ValueType) -> bool {
    match t {
        ir::ValueType::Float { .. } => true,
        ir::ValueType::Pointer(i)
        | ir::ValueType::Array(i, _)
        | ir::ValueType::Vector(i, _)
        | ir::ValueType::Complex(i) => ir_type_uses_float(i),
        ir::ValueType::Function { ret, params } => {
            ir_type_uses_float(ret) || params.iter().any(ir_type_uses_float)
        }
        _ => false,
    }
}

fn ir_expr_uses_float(e: &ir::TypedExpr) -> bool {
    ir_type_uses_float(&e.ty)
        || matches!(e.literal, Some(ir::ExprLiteral::Float(_)))
        || e.children.iter().any(ir_expr_uses_float)
}

fn ir_stmt_uses_float(s: &ir::TypedStmt) -> bool {
    s.expressions.iter().any(ir_expr_uses_float) || s.nested.iter().any(ir_stmt_uses_float)
}

/// 把预处理器阶段产生的非致命诊断打印到 stderr。
/// PCH 不兼容 (W0400)、PCH 加载回退 (W0401)、缓存写失败
/// (W0402) 等场景会被转储; 错误会通过调用方已有的错误检查路径处理
/// (Err(pp_diags) → main.rs 输出)。
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
///
/// 交叉编译时, 若未显式指定 --sysroot, 自动搜索常见 sysroot 路径:
///   - /usr/<target-triple>  (如 /usr/aarch64-linux-gnu)
///   - /usr/local/<target-triple>
fn apply_sysroot(mut cfg: Config) -> Config {
    // Auto-discover sysroot for cross-compilation when not explicitly set
    if cfg.sysroot.is_none() {
        let host = crate::cli::TargetTriple::host();
        let is_cross = cfg.target.arch != host.arch || cfg.target.os != host.os;
        if is_cross {
            let triple_str = cfg.target.as_str();
            let candidates = [
                format!("/usr/{}", triple_str),
                format!("/usr/local/{}", triple_str),
            ];
            for candidate in &candidates {
                let p = std::path::Path::new(candidate);
                if p.is_dir() {
                    if cfg.verbose {
                        eprintln!("dcc: auto sysroot = {}", p.display());
                    }
                    cfg.sysroot = Some(p.to_path_buf());
                    break;
                }
            }
        }
    }

    let Some(sr) = cfg.sysroot.clone() else {
        return cfg;
    };
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

    // 1) PGO: 读 .profraw 或 .profdata (GCC/Clang 风格合并后的 profile)
    //    先按 profraw ("PROF" magic) 尝试; 若失败则按 profdata ("PRFD" magic)
    //    尝试, 与 "compile → run → profraw → merge/profdata → profile-use"
    //    端到端流程对齐.
    let pgo_w = prof
        .pgo_use
        .as_ref()
        .and_then(|p| {
            profile::pgo::Profraw::read(p)
                .or_else(|_| profile::pgo::Profraw::read_profdata(p))
                .ok()
                .map(|p| profile::pgo::PgoWeights::from_profraw(&p))
        })
        .unwrap_or_default();

    // 2) AutoFDO
    let autofdo = prof
        .auto_profile
        .as_ref()
        .or(prof.sample_use.as_ref())
        .and_then(|p| profile::autofdo::AutoFdoProfile::read(p).ok())
        .unwrap_or_default();

    // 3) LBR
    let lbr = prof
        .lbr_profile
        .as_ref()
        .and_then(|p| profile::lbr::LbrProfile::read_perf_brstack(p).ok())
        .unwrap_or_default();

    // 4) MemProf
    let memprof = prof
        .memprof_use
        .as_ref()
        .and_then(|p| profile::memprof::MemProfData::read(p).ok())
        .unwrap_or_default();

    let weights = profile::weights::ProfileWeights::from_sources(&pgo_w, &autofdo, &lbr, &memprof);

    let hot = weights.hot_funcs();
    let cold = weights.cold_funcs();
    for d in &mut prog.decls {
        if let Decl::Func(f) = d {
            if f.body.is_none() {
                continue;
            }
            if hot.contains(&f.name.name) {
                f.is_hot = true;
            }
            if cold.contains(&f.name.name) {
                f.is_cold = true;
            }
        }
    }
}

pub fn run(cfg: Config) -> Result<(), Vec<Diagnostic>> {
    // --sysroot= 重写 include / library 路径. 必须在最早期调用, 让后续
    // 预处理器 / 链接器都看到正确路径.
    let cfg = apply_sysroot(cfg);

    // 输出 CLI 解析期间收集的警告 (未知/忽略的标志)
    for w in &cfg.cli_warnings {
        eprintln!("dcc: warning: {}", w);
    }

    // -r 多 .o 可重定位链接（host ld -r 兼容路径）
    // 支持 -T script.lds + -r 组合 (内核模块构建需要)
    if cfg.link_mode && !cfg.built_link {
        let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
        return crate::link::legacy::link_relocatable_with_script(
            &cfg.inputs,
            &cfg.output,
            lto_enabled,
            cfg.verbose,
            cfg.linker_script.as_deref(),
        );
    }

    // -pie / -shared / -fbuiltin-linker / .o 自动链接：走内置全链接器
    if cfg.built_link {
        return run_builtin_linker(&cfg);
    }

    // Multi-file parallel compilation: -c/-S/-E/-M/-MM/-fsyntax-only with
    // multiple inputs compiles each file independently in parallel.
    if cfg.inputs.len() > 1
        && (cfg.relocatable
            || cfg.emit_asm
            || cfg.preprocess_only
            || cfg.dep_no_compile
            || cfg.syntax_only)
    {
        return compile_multiple_files(&cfg);
    }

    let input = cfg.inputs.first().ok_or_else(|| {
        vec![Diagnostic::err(
            "E0100",
            "no input file",
            crate::diagnostics::Span::point("", 0, 0),
        )]
    })?;

    // -x assembler / assembler-with-cpp 以及 .s/.S 使用系统汇编器
    if matches!(
        cfg.input_lang,
        InputLang::Assembler | InputLang::AssemblerWithCpp
    ) || input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e, "s" | "S"))
        .unwrap_or(false)
    {
        return run_assembler_driver(&cfg, input);
    }

    // C++ 前端: -x c++ / .cpp / .cc / .cxx / .CPP / .CXX
    let is_cpp = matches!(cfg.input_lang, InputLang::Cpp)
        || input
            .extension()
            .and_then(|e| e.to_str())
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
        let host = crate::cli::TargetTriple::host();
        let is_cross = cfg.target.arch != host.arch || cfg.target.os != host.os;
        if is_cross {
            eprintln!("dcc: target = {} (cross)", cfg.target.as_str());
        } else {
            eprintln!("dcc: target = {}", cfg.target.as_str());
        }
        eprintln!("dcc: std    = {}", cfg.std.as_str());
        eprintln!("dcc: -O{}", cfg.opt);
        if cfg.debug_level > 0 {
            eprintln!("dcc: -g{}", cfg.debug_level);
        }
        if cfg.lto_enabled {
            eprintln!("dcc: -flto");
        }
    }

    // 增量编译: 检查源文件是否未变更, 若缓存命中则跳过编译.
    // 在 -fcached 模式下, 基于 (源内容 + 编译标志) 的哈希判断是否需要重编译.
    let incr_cache = IncrementalCache::new(cfg.verbose);
    let incr_entry_path = if incremental::should_use_incremental(&cfg, input) {
        match incr_cache.check(&cfg, input) {
            CacheResult::Hit => {
                // 缓存命中: 输出已是最新, 直接返回
                return Ok(());
            }
            CacheResult::Miss { entry_path } => Some(entry_path),
        }
    } else {
        None
    };

    // 读源：mmap（ADR-006）或 stdin
    let (mapped, file_name) = if input.as_os_str() == "-" {
        let mapped = MappedInput::from_stdin(io::stdin()).map_err(|e| {
            vec![Diagnostic::err(
                "E0100",
                format!("cannot read stdin: {}", e),
                crate::diagnostics::Span::point("-", 0, 0),
            )]
        })?;
        (mapped, "-".to_string())
    } else {
        let mapped = MappedInput::open(input).map_err(|e| {
            vec![Diagnostic::err(
                "E0100",
                format!("cannot read {}: {}", input.display(), e),
                crate::diagnostics::Span::point(&input.display().to_string(), 0, 0),
            )]
        })?;
        (mapped, input.display().to_string())
    };
    let src = mapped.as_slice();

    // 预处理
    let pp_out = preproc::preprocess(
        src,
        &file_name,
        &cfg.include_paths,
        &cfg.system_paths,
        &cfg.quote_paths,
        &cfg.after_paths,
        &cfg.defines,
        cfg.std,
        &cfg.force_includes,
        cfg.freestanding,
        &cfg.pch_opts,
        cfg.kernel_code_model,
        cfg.short_wchar,
        &cfg.target,
        &cfg.macro_prefix_maps,
        cfg.macro_backtrace_limit,
        cfg.trigraphs,
        cfg.warn_undef,
        cfg.opt,
    );
    let mut pp_diags = pp_out.diags;
    let preprocessed = pp_out.text;
    let line_map = pp_out.line_map;
    let pack_history = pp_out.pack_history;
    let weak_symbols = pp_out.weak_symbols;
    let include_deps = pp_out.deps;
    let pp_macros = pp_out.macros;
    let fenv_access = pp_out.fenv_access;
    let fp_contract = pp_out.fp_contract;
    let cx_limited_range = pp_out.cx_limited_range;
    enrich_diags(&mut pp_diags, src, &file_name, input.parent());
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        return Err(pp_diags);
    }
    // 把预处理器阶段的非致命诊断 (PCH 不兼容 / 缓存跳过等) 打印到 stderr.
    // 这与 -E 模式共享同一路径, 即使 -E 也会显示 PCH 警告.
    emit_preproc_warnings(&pp_diags);

    // -E 模式：只做预处理，输出到 -o 指定文件或 stdout
    if cfg.preprocess_only {
        // 构建输出目标: -o 显式指定时写文件, 否则写 stdout.
        // -o - 与不指定 -o 一样走 stdout (与 GCC 行为一致).
        let mut pp_out: Box<dyn Write> = if cfg.output_explicit && cfg.output.to_str() != Some("-")
        {
            let path = &cfg.output;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            Box::new(fs::File::create(path).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", path.display(), e),
                    crate::diagnostics::Span::point(&path.display().to_string(), 0, 0),
                )]
            })?)
        } else {
            Box::new(io::stdout())
        };
        write!(pp_out, "{}", preprocessed).map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("write error: {}", e),
                crate::diagnostics::Span::point("", 0, 0),
            )]
        })?;
        // -dM: 额外输出所有宏定义 (供 kernel 探测编译器用)
        if cfg.dump_macros {
            // 按宏名字典序输出，便于差异比较
            let mut sorted = pp_macros;
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, params, variadic, body) in &sorted {
                match params {
                    None => writeln!(pp_out, "#define {} {}", name, body).ok(),
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
                        writeln!(pp_out, "#define {}({}) {}", name, p_str, body).ok()
                    }
                };
            }
        }
        // -E 模式下也需要生成依赖文件 (如 kernel 的 module.lds.S 预处理)
        if cfg.gen_dep {
            let dep_path = cfg.dep_file.as_ref().unwrap_or(&cfg.output);
            let target = cfg
                .dep_target
                .as_deref()
                .unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
            let target_owned;
            let target_ref = if cfg.dep_quote_target {
                target_owned = quote_make_target(target);
                &target_owned
            } else {
                target
            };
            let deps: Vec<String> = if cfg.dep_mmd_only {
                filter_user_deps(include_deps, &cfg.system_paths)
            } else {
                include_deps
            };
            if dep_path.to_str() != Some("-") {
                if let Some(parent) = dep_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                let mut dep_out = fs::File::create(dep_path).map_err(|e| {
                    vec![Diagnostic::err(
                        "E0101",
                        format!("cannot write {}: {}", dep_path.display(), e),
                        crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                    )]
                })?;
                write_dep_file(
                    &mut dep_out,
                    target_ref,
                    &input.display().to_string(),
                    &deps,
                    cfg.dep_phony,
                )
                .map_err(|e| {
                    vec![Diagnostic::err(
                        "E0101",
                        format!("cannot write dep file: {}", e),
                        crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                    )]
                })?;
            }
        }
        return Ok(());
    }

    // -M/-MM 模式: 仅生成依赖文件, 不编译 (不进入 -E 预处理输出路径)
    if cfg.dep_no_compile {
        // -M/-MM 默认输出到 stdout, 除非 -MF 显式指定文件
        let mut dep_out: Box<dyn Write> = match cfg.dep_file.as_ref() {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                Box::new(fs::File::create(path).map_err(|e| {
                    vec![Diagnostic::err(
                        "E0101",
                        format!("cannot write {}: {}", path.display(), e),
                        crate::diagnostics::Span::point(&path.display().to_string(), 0, 0),
                    )]
                })?)
            }
            None => Box::new(io::stdout()),
        };
        let default_target = input
            .file_stem()
            .map(|s| format!("{}.o", s.to_string_lossy()))
            .unwrap_or_else(|| "unknown".to_string());
        let target = cfg.dep_target.as_deref().unwrap_or(&default_target);
        let target_owned;
        let target_ref = if cfg.dep_quote_target {
            target_owned = quote_make_target(target);
            &target_owned
        } else {
            target
        };
        let deps: Vec<String> = if cfg.dep_mmd_only {
            filter_user_deps(include_deps, &cfg.system_paths)
        } else {
            include_deps
        };
        write_dep_file(
            dep_out.as_mut(),
            target_ref,
            &input.display().to_string(),
            &deps,
            cfg.dep_phony,
        )
        .map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("cannot write dep file: {}", e),
                crate::diagnostics::Span::point("", 0, 0),
            )]
        })?;
        return Ok(());
    }

    // 解析
    let allow_implicit_function_decl = cfg.permissive
        || matches!(
            cfg.std,
            crate::cli::StdMode::C89
                | crate::cli::StdMode::Gnu89
                | crate::cli::StdMode::Gnu99
                | crate::cli::StdMode::Gnu11
                | crate::cli::StdMode::Gnu17
                | crate::cli::StdMode::Gnu23
        );
    // GNU 模式允许隐式 int 返回类型 (C89 遗留特性, GCC 始终保留)
    let allow_implicit_int = cfg.permissive
        || matches!(
            cfg.std,
            crate::cli::StdMode::Gnu89
                | crate::cli::StdMode::Gnu99
                | crate::cli::StdMode::Gnu11
                | crate::cli::StdMode::Gnu17
                | crate::cli::StdMode::Gnu23
        );
    let (mut prog, mut diags) = parser::parse_with_opts(
        preprocessed.as_bytes(),
        &file_name,
        parser::ParseOptions {
            permissive: cfg.permissive,
            pack_history: pack_history.clone(),
            weak_symbols: weak_symbols.clone(),
            warn_int_truncation: cfg.warn_int_truncation,
            warn_int_conversion: cfg.warn_int_conversion,
            is_c23: matches!(
                cfg.std,
                crate::cli::StdMode::C23 | crate::cli::StdMode::Gnu23
            ),
            allow_implicit_function_decl,
            allow_implicit_int,
            warn_implicit_decl: cfg.warn_implicit_decl,
            cuda: cfg.profile.offload.cuda,
            hip: cfg.profile.offload.hip,
            fenv_access,
            fp_contract,
            cx_limited_range,
            short_wchar: cfg.short_wchar,
            is_lp64: matches!(
                cfg.target.arch,
                crate::cli::Arch::X86_64
                    | crate::cli::Arch::AArch64
                    | crate::cli::Arch::RiscV64
                    | crate::cli::Arch::Mips64
            ),
        },
    );
    drop(preprocessed);
    drop(pack_history);
    drop(weak_symbols);
    // 把诊断的 span 从预处理文本行号映射回源文件位置
    remap_diags(&mut diags, &line_map);
    let dcfg = diag_cfg(&cfg);
    enrich_diags(&mut diags, src, &file_name, input.parent());
    // 警告由 main.rs 统一输出 (print_all 含去重), 避免重复;
    // 但无错误时需立即输出, 否则 warning-only 场景会丢失
    let has_errors = diags.iter().any(|d| matches!(d.sev, Severity::Error));
    if !has_errors {
        for d in &diags {
            if matches!(d.sev, Severity::Warning) {
                d.print_with(&dcfg);
            }
        }
    }
    if has_errors {
        return Err(diags);
    }

    let mut wlist: Vec<Diagnostic> = format_check::check_program(&prog);
    // 把诊断 span 从预处理输出行号映射回源文件位置, 否则内核等
    // 包含大量头文件的 TU 会把 50 万行的预处理物理行号直接打到 stderr.
    remap_diags(&mut wlist, &line_map);
    // 填充 source_line (主文件或对应头文件), 使 caret 指向真实源码.
    enrich_diags(&mut wlist, src, &file_name, input.parent());
    for w in &wlist {
        w.print_with(&dcfg);
        diags.push(w.clone());
    }
    drop(wlist);

    // Offloading: 收集 OpenMP target/simd + CUDA/HIP 限定符函数为
    // offload entries, 构造 `.dcc.offload` 节内容。校验错误/警告均带
    // 精确源位置 (经 line_map 映射回源文件)。
    let (offload_section, mut offload_diags) = collect_offload_section(
        &prog,
        &cfg.profile.offload,
        &pp_out.openmp_pragmas,
        &line_map,
    );
    // NOTE: collect_offload_section 内部已完成所有 remap:
    //   - CUDA/HIP 诊断使用 AST span (预处理坐标), 在函数内 remap
    //   - OpenMP 诊断使用 openmp_pragmas 的源行号, 已是源坐标
    // 此处不再调用 remap_diags, 避免对已是源坐标的 OpenMP 诊断
    // 做二次映射导致行号偏移。
    enrich_diags(&mut offload_diags, src, &file_name, input.parent());
    let offload_has_err = offload_diags
        .iter()
        .any(|d| matches!(d.sev, Severity::Error));
    if offload_has_err {
        // 错误由 main.rs 统一打印, 此处只收集, 避免重复输出
        diags.extend(offload_diags);
        return Err(diags);
    }
    // 无错误: 打印 offload 收集阶段的警告 (带精确位置)
    for d in &offload_diags {
        if matches!(d.sev, Severity::Warning) {
            d.print_with(&dcfg);
        }
    }

    // Offloading: 设备代码生成 (PTX / AMDGCN)
    // 将 offload entry 编译为设备侧 kernel 汇编, 错误作为硬诊断
    // 加入 offload_diags (与 entry 收集阶段的错误统一处理)。
    if let Some(ref section) = offload_section {
        let dcg_result = profile::device_codegen::generate_device_code(section);
        for err in &dcg_result.errors {
            offload_diags.push(Diagnostic::err(
                "E0320",
                err.clone(),
                crate::diagnostics::Span::point("", 0, 0),
            ));
        }
        if cfg.verbose {
            for (triple, code) in &dcg_result.device_code {
                eprintln!(
                    "dcc: offload device codegen: {} -> {} bytes",
                    triple,
                    code.len()
                );
            }
            for d in &dcg_result.diagnostics {
                eprintln!("dcc: offload device codegen: {}", d);
            }
        }
    }

    // sanitizer 插桩（AST 层，在 IR lower 之前）
    sanitizer::instrument_program(&mut prog, cfg.sanitizers, cfg.target.arch);

    // IPA-SRA: 跨过程标量替换 (仅 static 函数, 在 IR lower 之前)
    // -O2 及以上默认启用; -fipa-sra / -fno-ipa-sra 显式控制
    if cfg.ipa_sra.should_run() {
        crate::ipa_sra::run_ipa_sra(&mut prog, &cfg.ipa_sra);
    }

    // PGO 集成: 加载 profile 数据, 构建 PGO 集成上下文
    // 为 IPA-SRA / 内联 / 函数布局提供 profile 感知的决策
    let _pgo_ctx = if let Some(pgo_path) = cfg.profile.pgo_use.as_ref() {
        if !(cfg.profile.pgo_inline || cfg.profile.pgo_ipa_sra) {
            None
        } else {
            let pgo_weights = crate::profile::pgo::PgoWeights::from_profraw(
                &crate::profile::pgo::Profraw::read(pgo_path).unwrap_or_default(),
            );
            let profile_weights = crate::profile::weights::ProfileWeights::from_sources(
                &pgo_weights,
                &crate::profile::autofdo::AutoFdoProfile::default(),
                &crate::profile::lbr::LbrProfile::default(),
                &crate::profile::memprof::MemProfData::default(),
            );
            let pgo_cfg = crate::profile::pgo_integration::PgoIntegrationConfig {
                enabled: true,
                opt_level: cfg.opt,
                profile_path: cfg
                    .profile
                    .pgo_use
                    .as_ref()
                    .map(|p| p.display().to_string()),
                inline_hints: cfg.profile.pgo_inline,
                ipa_sra_hints: cfg.profile.pgo_ipa_sra,
                hot_threshold: cfg.profile.pgo_hot_threshold,
                cold_threshold: cfg.profile.pgo_cold_threshold,
            };
            let ctx = crate::profile::pgo_integration::PgoIntegrationContext::from_sources(
                &pgo_weights,
                &profile_weights,
                &pgo_cfg,
            );
            // 应用 hot/cold 标记到函数声明
            crate::profile::pgo_integration::apply_pgo_hotness(&mut prog, &ctx);
            Some(ctx)
        }
    } else {
        None
    };

    // PGO 插桩（AST 层）: -fprofile-generate / -fprofile-values
    // 在每个函数体顶部插入 counter/value sampling 调用。编译时同时
    // 落盘 `<stem>.pgort.c`, 运行后由内置 runtime 写 profraw v2。
    if cfg.profile.pgo_generate.is_some() || cfg.profile.value_sampling {
        let pgo_cfg = profile::pgo::PgoConfig {
            generate: cfg.profile.pgo_generate.is_some(),
            value_sampling: cfg.profile.value_sampling,
            use_file: cfg
                .profile
                .pgo_use
                .as_ref()
                .map(|p| p.display().to_string()),
            out_dir: cfg
                .profile
                .pgo_generate
                .as_ref()
                .map(|p| p.display().to_string()),
        };
        profile::pgo::instrument_program(&mut prog, &pgo_cfg);
        if !cfg.syntax_only {
            let pgo_stem = input
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("a")
                .to_string();
            let profraw_dir = cfg
                .profile
                .pgo_generate
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let profraw = profraw_dir.join(format!("{pgo_stem}.profraw"));
            if let Some(parent) = profraw.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            let pgo_rt = crate::profile::pgo::PgoRuntime::generate_c(&prog, &file_name, &profraw);
            let rt_path = PathBuf::from(format!("{pgo_stem}.pgort.c"));
            if let Err(e) = fs::write(&rt_path, pgo_rt) {
                return Err(vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", rt_path.display(), e),
                    crate::diagnostics::Span::point(&rt_path.display().to_string(), 0, 0),
                )]);
            }
        }
    }

    // GCOV: -ftest-coverage (notes) / -fprofile-arcs (arc counters)。
    // 在每个函数的基本块入口插入 `__dcc_gcov_counter_inc(idx)` 调用,
    // 同时落盘 `.gcno` 与运行时 C (`<stem>.gcovrt.c`)。程序运行后由
    // 运行时在 atexit 阶段把真实 counter 写成 `.gcda`, 与 GCC libgcov
    // 行为一致。仅 `-fsyntax-only` 时不产出任何文件。
    if (cfg.profile.gcov || cfg.profile.profile_arcs) && !cfg.syntax_only {
        let gcov_cfg = crate::profile::gcov::GcovConfig {
            emit_notes: cfg.profile.gcov,
            emit_data: cfg.profile.profile_arcs,
            counter_bits: if cfg.profile.profile_arcs { 64 } else { 32 },
        };
        let mut gcov_unit =
            crate::profile::gcov::instrument_program(&mut prog, &file_name, &gcov_cfg);
        // 行号表从预处理输出坐标映射回源文件坐标
        remap_gcov_lines(&mut gcov_unit, &line_map);
        let gcov_stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("a")
            .to_string();
        if cfg.profile.gcov {
            let gcno = PathBuf::from(format!("{gcov_stem}.gcno"));
            if let Err(e) = gcov_unit.write_notes(&gcno) {
                return Err(vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", gcno.display(), e),
                    crate::diagnostics::Span::point(&gcno.display().to_string(), 0, 0),
                )]);
            }
        }
        // 运行时 C 始终生成: 没有它 counter 自增调用无法链接
        let gcov_rt = crate::profile::gcov::GcovRuntime::generate_c(&gcov_unit);
        let rt_path = PathBuf::from(format!("{gcov_stem}.gcovrt.c"));
        if let Err(e) = fs::write(&rt_path, gcov_rt) {
            return Err(vec![Diagnostic::err(
                "E0101",
                format!("cannot write {}: {}", rt_path.display(), e),
                crate::diagnostics::Span::point(&rt_path.display().to_string(), 0, 0),
            )]);
        }
    }

    // MemProf: -fmemory-profile 时落盘运行时 C (`<stem>.memprofc.c`)。
    // 运行时实现 callstack 采集 + lifetime 配对 + .memprofdata 写出,
    // 供链接到被测程序 (插桩改写 malloc/free 为 __dcc_memprof_*_record)。
    // 仅 -fsyntax-only 时不产出文件。
    if cfg.profile.memprof && !cfg.syntax_only {
        let memprof_stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("a")
            .to_string();
        let memprof_rt = crate::profile::memprof::MemProfRuntime::generate_runtime();
        let rt_path = PathBuf::from(format!("{memprof_stem}.memprofc.c"));
        if let Err(e) = fs::write(&rt_path, memprof_rt) {
            return Err(vec![Diagnostic::err(
                "E0101",
                format!("cannot write {}: {}", rt_path.display(), e),
                crate::diagnostics::Span::point(&rt_path.display().to_string(), 0, 0),
            )]);
        }
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
        let target = cfg
            .dep_target
            .as_deref()
            .unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
        let target_owned;
        let target_ref = if cfg.dep_quote_target {
            target_owned = quote_make_target(target);
            &target_owned
        } else {
            target
        };
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
            Box::new(fs::File::create(dep_path).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", dep_path.display(), e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                )]
            })?)
        };
        write_dep_file(
            dep_out.as_mut(),
            target_ref,
            &input.display().to_string(),
            &deps,
            cfg.dep_phony,
        )
        .map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("cannot write dep file: {}", e),
                crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
            )]
        })?;
    }

    // AST -> typed IR -> validation
    let (mut module, ir_warnings) =
        ir::lower(prog).map_err(|d| remap_enrich(d, &line_map, src, &file_name, input.parent()))?;
    // Print IR-lowering warnings (e.g. W1004/W1009/W1010 from implicit
    // function declarations) after remapping spans back to source.
    let ir_warnings = remap_enrich(ir_warnings, &line_map, src, &file_name, input.parent());
    for w in &ir_warnings {
        w.print_with(&dcfg);
    }
    diags.extend(ir_warnings);
    ir::validate(&module)
        .map_err(|d| remap_enrich(d, &line_map, src, &file_name, input.parent()))?;
    // Compute target info for optimizer (VNNI vectorization etc.)
    let opt_has_vnni = cfg.cpu_feature_overrides.avx_vnni == Some(true)
        || cfg.march.as_deref().map_or(false, |m| {
            matches!(m, "x86-64-v4" | "sapphirerapids" | "granite-rapids" | "icelake-server" | "icelake-client")
        });
    let opt_vec_bits: u32 = if cfg.cpu_feature_overrides.avx512f == Some(true)
        || cfg.march.as_deref().map_or(false, |m| m == "x86-64-v4")
    {
        512
    } else if cfg.cpu_feature_overrides.avx2 == Some(true)
        || cfg.march.as_deref().map_or(false, |m| {
            matches!(m, "x86-64-v3" | "x86-64-v4" | "haswell" | "skylake" | "skylake-avx512")
        })
    {
        256
    } else {
        0
    };
    crate::opt::optimize_with_full_target(
        &mut module,
        cfg.opt,
        opt_has_vnni,
        opt_vec_bits,
        cfg.strict_aliasing,
    );
    if cfg.opt > 0 {
        ir::resync(&mut module)
            .map_err(|d| remap_enrich(d, &line_map, src, &file_name, input.parent()))?;
        ir::validate(&module)
            .map_err(|d| remap_enrich(d, &line_map, src, &file_name, input.parent()))?;
    }

    let mut asm_diags = codegen::validate_inline_asm_constraints(&module);
    if !asm_diags.is_empty() {
        remap_diags(&mut asm_diags, &line_map);
        enrich_diags(&mut asm_diags, src, &file_name, input.parent());
        let asm_has_err = asm_diags.iter().any(|d| matches!(d.sev, Severity::Error));
        if asm_has_err {
            return Err(asm_diags);
        }
        for d in &asm_diags {
            if matches!(d.sev, Severity::Warning) {
                d.print_with(&dcfg);
            }
        }
        diags.extend(asm_diags);
    }

    if cfg.syntax_only {
        return Ok(());
    }

    // codegen
    let mut target = Target::resolve(&cfg.target);
    target.apply_codegen_options(cfg.red_zone, cfg.frame_pointer, cfg.sse);
    // CPU 指令集特性: 应用 CLI 显式覆盖 (在 CPUID 动态检测基础上)
    target.apply_cpu_feature_overrides(&cfg.cpu_feature_overrides);
    // -march/-mcpu: 应用微架构特性组合
    if let Some(ref arch_name) = cfg.march {
        target.apply_march(arch_name);
    }
    if let Some(ref cpu_name) = cfg.mcpu {
        target.apply_march(cpu_name);
    }
    if let Some(ref tune_name) = cfg.mtune {
        target.mtune = Some(tune_name.clone());
    }
    // ARM instruction mode (-mthumb/-marm)
    target.arm_instruction_mode = cfg.arm_instruction_mode;
    // Float ABI mode (-mfloat-abi=)
    target.float_abi = cfg.float_abi;
    if cfg.verbose && cfg.target.arch == crate::cli::Arch::X86_64 {
        let feats = target.cpu_features.to_feature_strings();
        if !feats.is_empty() {
            eprintln!("dcc: cpu features: {}", feats.join(", "));
        }
    }
    target.kernel_code_model = cfg.kernel_code_model;
    target.auto_var_init_zero = cfg.auto_var_init_zero;
    target.set_stack_protector(cfg.stack_protector);
    target.unwind_tables = cfg.unwind_tables;
    target.function_return_thunk_extern = cfg.function_return_thunk_extern;
    target.indirect_branch_thunk_extern = cfg.indirect_branch_thunk_extern;
    target.patchable_function_entry = cfg.patchable_function_entry;
    target.ftrace_mode = cfg.ftrace_mode;
    target.stack_protector_guard_reg = cfg.stack_protector_guard_reg;
    target.stack_clash_protection = cfg.stack_clash_protection;
    // -fno-strict-aliasing: 优化器当前不做类型双关假设 (TBAA), 标志
    // 存储于 target 供未来优化 pass 参考.
    target.strict_aliasing = cfg.strict_aliasing;
    // -fno-strict-overflow: 优化器当前不假设有符号溢出为 UB, 常量折叠
    // 使用 wrapping 算术. 标志存储于 target 供未来优化 pass 参考.
    target.strict_overflow = cfg.strict_overflow;
    // -ffast-math: 激进浮点优化
    target.fast_math = cfg.fast_math;
    // -ffp-contract=: FP 收缩模式
    target.fp_contract_mode = cfg.fp_contract_mode;
    // -falign-loops/-falign-jumps: 代码段对齐
    target.align_loops = cfg.align_loops;
    target.align_jumps = cfg.align_jumps;
    target.short_wchar = cfg.short_wchar;
    // -fno-common: codegen 已将未初始化全局变量直接放入 .bss (而非
    // COMMON 段). 链接器 resolve.rs 处理跨 TU tentative definition 合并.
    target.no_common = cfg.no_common;
    if !target.sse {
        // 精确检测 IR 中是否真的使用浮点类型/字面量, 避免把含
        // double_xxx 标识符(如内核 double_rq_lock)的 Debug 串误判为浮点
        // 找到第一个使用浮点的函数或全局, 用其精确 span 报错
        let sse_span = module
            .functions
            .iter()
            .find(|f| {
                ir_type_uses_float(&f.return_type)
                    || f.params.iter().any(ir_type_uses_float)
                    || f.body.iter().any(ir_stmt_uses_float)
            })
            .map(|f| f.span.clone())
            .or_else(|| {
                module
                    .globals
                    .iter()
                    .find(|g| {
                        ir_type_uses_float(&g.ty)
                            || g.initializer.as_ref().is_some_and(ir_expr_uses_float)
                    })
                    .map(|g| g.span.clone())
            });
        if let Some(mut span) = sse_span {
            // 将 span 从预处理文本行号映射回源文件
            remap_span(&mut span, &line_map);
            // 内核 FPU 子系统显式使用 float/double, 即使 -mno-sse 也应允许编译
            // 降级为警告而非硬错误, 让后端处理代码生成
            let mut w = vec![Diagnostic::warn(
                "W0300",
                "floating-point code with -mno-sse; x87 may be used for FP operations",
                span,
            )];
            enrich_diags(&mut w, src, &file_name, input.parent());
            for d in &w {
                d.print_with(&dcfg);
            }
            diags.extend(w);
            // 不阻断编译, 继续后续流程
        }
    }

    let lto_enabled = cfg.lto_enabled || lto::is_lto_enabled(&cfg.raw_args);
    let function_sections = cfg.function_sections || lto_enabled;
    let bytes = codegen::emit(
        &module,
        &target,
        cfg.relocatable,
        cfg.emit_asm,
        &file_name,
        cfg.debug_level,
        cfg.dwarf_version,
        cfg.sanitizers,
        lto_enabled,
        function_sections,
        cfg.parallel_codegen,
        cfg.bounds_check,
        cfg.trapv,
        offload_section.as_ref(),
        &line_map,
        cfg.verbose,
    )?;
    if cfg.verbose {
        eprintln!("dcc: size   = {} bytes", bytes.len());
    }

    let out = &cfg.output;
    // -o -: 写 stdout (管道输出), 与 GCC 行为一致
    if out.to_str() == Some("-") {
        io::stdout().write_all(&bytes).map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("write error (stdout): {}", e),
                crate::diagnostics::Span::point("", 0, 0),
            )]
        })?;
    } else {
        let mut f = fs::File::create(out).map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("cannot write {}: {}", out.display(), e),
                crate::diagnostics::Span::point(&out.display().to_string(), 0, 0),
            )]
        })?;
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
    }

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

    // 增量编译: 编译成功后更新缓存条目
    if let Some(entry_path) = incr_entry_path {
        incr_cache.update(&cfg, input, &entry_path);
    }

    Ok(())
}

/// Multi-file parallel compilation: compile each input file independently.
///
/// Uses `std::thread::scope` (consistent with existing function-level
/// parallel codegen in `codegen/x86_64.rs`). Worker count is controlled
/// by `-j`/`--jobs`; defaults to `available_parallelism()`.
///
/// Each worker derives per-file output/dep names from the input stem,
/// clones the `Config`, and calls `run()` for the single file.
fn compile_multiple_files(cfg: &Config) -> Result<(), Vec<Diagnostic>> {
    let n = cfg.inputs.len();
    let force_single = cfg.jobs == Some(1);
    let degree = if force_single {
        1
    } else {
        let max_workers = cfg.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4)
        });
        max_workers.min(n)
    };

    if cfg.verbose {
        eprintln!("dcc: compiling {} files with {} worker(s)", n, degree);
    }

    // Sequential fallback for -j1 or single effective worker
    if degree <= 1 {
        let mut all_diags: Vec<Diagnostic> = Vec::new();
        for input in &cfg.inputs {
            let mut c = cfg.clone();
            c.inputs = vec![input.clone()];
            let stem = input
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if cfg.emit_asm {
                c.output = PathBuf::from(format!("{}.s", stem));
            } else if cfg.relocatable {
                c.output = PathBuf::from(format!("{}.o", stem));
            }
            // Per-file dep file (only for -MD/-MMD, not -M/-MM)
            if cfg.gen_dep && !cfg.dep_no_compile {
                c.dep_file = Some(PathBuf::from(format!("{}.d", stem)));
                c.dep_target = Some(c.output.display().to_string());
            }
            if let Err(diags) = run(c) {
                all_diags.extend(diags);
            }
        }
        if all_diags.is_empty() {
            return Ok(());
        } else {
            return Err(all_diags);
        }
    }

    // Parallel compilation using std::thread::scope
    let next_job = std::sync::atomic::AtomicUsize::new(0);
    let slots = std::sync::Mutex::new(vec![None; n]);
    std::thread::scope(|scope| {
        for _ in 0..degree {
            scope.spawn(|| loop {
                let idx = next_job.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if idx >= n {
                    break;
                }
                let input = &cfg.inputs[idx];
                let mut c = cfg.clone();
                c.inputs = vec![input.clone()];

                let stem = input
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if cfg.emit_asm {
                    c.output = PathBuf::from(format!("{}.s", stem));
                } else if cfg.relocatable {
                    c.output = PathBuf::from(format!("{}.o", stem));
                }
                if cfg.gen_dep && !cfg.dep_no_compile {
                    c.dep_file = Some(PathBuf::from(format!("{}.d", stem)));
                    c.dep_target = Some(c.output.display().to_string());
                }

                let result = run(c);
                slots.lock().unwrap()[idx] = Some(result);
            });
        }
    });

    let results: Vec<Result<(), Vec<Diagnostic>>> = slots
        .into_inner()
        .expect("parallel compilation mutex poisoned")
        .into_iter()
        .map(|r| r.expect("parallel compilation dropped a file"))
        .collect();

    // Collect results in input order; merge all diagnostics
    let mut all_diags: Vec<Diagnostic> = Vec::new();
    for result in results {
        if let Err(diags) = result {
            all_diags.extend(diags);
        }
    }
    if all_diags.is_empty() {
        Ok(())
    } else {
        Err(all_diags)
    }
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

/// 转义依赖文件路径中的 Make 特殊字符 (空格, `#`, `:` 等).
/// GCC 对这些字符用 `\` 前缀转义, 使 Make 能正确解析.
fn escape_dep_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for ch in path.chars() {
        match ch {
            ' ' | '\t' | '#' | ':' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// -MQ 目标名转义: 将 `$` 转义为 `$$`, 防止 Make 将其解释为变量引用.
/// GCC 的 -MQ 对目标名中的 `$`, `:`, ` ` 等 Make 特殊字符进行转义.
fn quote_make_target(target: &str) -> String {
    let mut out = String::with_capacity(target.len() + 8);
    for ch in target.chars() {
        match ch {
            '$' => out.push_str("$$"),
            ' ' | '\t' | '#' | ':' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// 写 GCC 兼容的 .d 依赖文件.
///
/// 格式:
/// ```text
/// target: source \
///   dep1 \
///   dep2 \
///   dep3
/// ```
///
/// - 用 `\` 行续符连接所有依赖, 使 Make 将其视为单条规则
/// - 行宽超过 72 字符时折行 (与 GCC 行为一致)
/// - 路径中特殊字符 (` `, `#`, `:`) 用 `\` 转义
/// - `-MP` 时为每个头文件生成 phony 目标
fn write_dep_file(
    out: &mut dyn Write,
    target: &str,
    source: &str,
    deps: &[String],
    phony: bool,
) -> io::Result<()> {
    let esc_source = escape_dep_path(source);

    // GCC 格式: 每个依赖独占一行, 用 `\` 续行符连接, 最后一个无续行符
    // ```
    // target: source \
    //   dep1 \
    //   dep2 \
    //   dep3
    // ```
    // 源文件是 `-` (stdin) 时不包含它, 避免 fixdep 尝试打开无效路径
    let include_source = source != "-";
    if deps.is_empty() {
        if include_source {
            writeln!(out, "{}: {}", target, esc_source)?;
        } else {
            writeln!(out, "{}:", target)?;
        }
    } else {
        // 第一行: target: source \ 或 target: \
        if include_source {
            writeln!(out, "{}: {} \\", target, esc_source)?;
        } else {
            writeln!(out, "{}: \\", target)?;
        }
        // 中间依赖:  dep \
        for dep in &deps[..deps.len() - 1] {
            let esc_dep = escape_dep_path(dep);
            writeln!(out, "  {} \\", esc_dep)?;
        }
        // 最后一个依赖 (无续行符)
        let esc_dep = escape_dep_path(&deps[deps.len() - 1]);
        writeln!(out, "  {}", esc_dep)?;
    }

    // -MP: 为每个依赖生成空 phony 目标 (防止删除头文件后 make 报错)
    if phony {
        for dep in deps {
            let esc_dep = escape_dep_path(dep);
            writeln!(out)?;
            writeln!(out, "{}:", esc_dep)?;
        }
    }
    Ok(())
}

/// 独立汇编驱动: 预处理 .S 后调用系统 `as`。
fn run_assembler_driver(cfg: &Config, input: &std::path::Path) -> Result<(), Vec<Diagnostic>> {
    let file_name = input.display().to_string();
    let src = fs::read(input).map_err(|e| {
        vec![Diagnostic::err(
            "E0100",
            format!("cannot read {}: {}", file_name, e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;

    let (preprocessed, include_deps, mut pp_diags) =
        if matches!(cfg.input_lang, InputLang::Assembler)
            || input.extension().and_then(|e| e.to_str()) == Some("s")
        {
            (
                String::from_utf8_lossy(&src).into_owned(),
                Vec::new(),
                Vec::new(),
            )
        } else {
            // GCC 兼容: 处理 .S 文件时自动定义 __ASSEMBLER__
            // 内核头文件用 #ifndef __ASSEMBLER__ 保护 C 代码
            let mut asm_defines = cfg.defines.clone();
            if !asm_defines.iter().any(|(k, _)| k == "__ASSEMBLER__") {
                asm_defines.push(("__ASSEMBLER__".to_string(), Some("1".to_string())));
            }

            let pp_out = preproc::preprocess(
                &src,
                &file_name,
                &cfg.include_paths,
                &cfg.system_paths,
                &cfg.quote_paths,
                &cfg.after_paths,
                &asm_defines,
                cfg.std,
                &cfg.force_includes,
                cfg.freestanding,
                &cfg.pch_opts,
                cfg.kernel_code_model,
                cfg.short_wchar,
                &cfg.target,
                &cfg.macro_prefix_maps,
                cfg.macro_backtrace_limit,
                cfg.trigraphs,
                cfg.warn_undef,
                cfg.opt,
            );
            (pp_out.text, pp_out.deps, pp_out.diags)
        };
    enrich_diags(&mut pp_diags, &src, &file_name, input.parent());
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        return Err(pp_diags);
    }
    emit_preproc_warnings(&pp_diags);

    if cfg.preprocess_only {
        // -E 模式: 写到 -o 指定的文件 (或 stdout)
        let mut pp_out: Box<dyn Write> = if cfg.output_explicit && cfg.output.to_str() != Some("-")
        {
            let path = &cfg.output;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            Box::new(fs::File::create(path).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", path.display(), e),
                    crate::diagnostics::Span::point(&path.display().to_string(), 0, 0),
                )]
            })?)
        } else {
            Box::new(io::stdout())
        };
        write!(pp_out, "{}", preprocessed).map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("write error: {}", e),
                crate::diagnostics::Span::point("", 0, 0),
            )]
        })?;
        // 生成依赖文件 (如 kernel 的 module.lds.S 预处理)
        if cfg.gen_dep {
            let dep_path = cfg.dep_file.as_ref().unwrap_or(&cfg.output);
            let target = cfg
                .dep_target
                .as_deref()
                .unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
            let target_owned;
            let target_ref = if cfg.dep_quote_target {
                target_owned = quote_make_target(target);
                &target_owned
            } else {
                target
            };
            let deps: Vec<String> = if cfg.dep_mmd_only {
                filter_user_deps(include_deps, &cfg.system_paths)
            } else {
                include_deps
            };
            if dep_path.to_str() != Some("-") {
                if let Some(parent) = dep_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                let mut dep_out = fs::File::create(dep_path).map_err(|e| {
                    vec![Diagnostic::err(
                        "E0101",
                        format!("cannot write {}: {}", dep_path.display(), e),
                        crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                    )]
                })?;
                write_dep_file(
                    &mut dep_out,
                    target_ref,
                    &input.display().to_string(),
                    &deps,
                    cfg.dep_phony,
                )
                .map_err(|e| {
                    vec![Diagnostic::err(
                        "E0101",
                        format!("cannot write dep file: {}", e),
                        crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                    )]
                })?;
            }
        }
        return Ok(());
    }

    let tmp_dir = std::env::temp_dir();
    let asm_path = tmp_dir.join(format!("dcc-asm-{}.s", std::process::id()));
    fs::write(&asm_path, preprocessed.as_bytes()).map_err(|e| {
        vec![Diagnostic::err(
            "E0101",
            format!("cannot write temp asm: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;

    let mut cmd = Command::new("as");
    // -m32 需要传 --32 给系统汇编器 (如 vdso32 构建)
    if cfg.m32 {
        cmd.arg("--32");
    }
    for opt in &cfg.as_options {
        for part in opt.split(',') {
            if !part.is_empty() {
                cmd.arg(part);
            }
        }
    }
    cmd.arg("-o").arg(&cfg.output).arg(&asm_path);
    // -o -: as 原生支持 -o - 输出到 stdout, 需要捕获 stdout 并转发
    let is_stdout = cfg.output.to_str() == Some("-");
    let output = cmd.output().map_err(|e| {
        vec![Diagnostic::err(
            "E0102",
            format!("cannot invoke assembler: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;
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
        return Err(vec![Diagnostic::err(
            "E0102",
            msg,
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]);
    }
    // -o -: 把 as 的 stdout 转发到 dcc 的 stdout
    if is_stdout {
        io::stdout().write_all(&output.stdout).map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("write error (stdout): {}", e),
                crate::diagnostics::Span::point("", 0, 0),
            )]
        })?;
    } else {
        eprintln!("dcc: wrote {}", cfg.output.display());
    }

    // .S 文件也需要生成依赖文件 (-MMD / -MD)
    if cfg.gen_dep {
        let dep_path = cfg.dep_file.as_ref().unwrap_or(&cfg.output);
        let target = cfg
            .dep_target
            .as_deref()
            .unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
        let target_owned;
        let target_ref = if cfg.dep_quote_target {
            target_owned = quote_make_target(target);
            &target_owned
        } else {
            target
        };
        let deps: Vec<String> = if cfg.dep_mmd_only {
            filter_user_deps(include_deps, &cfg.system_paths)
        } else {
            include_deps
        };
        if dep_path.to_str() != Some("-") {
            if let Some(parent) = dep_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            let mut dep_out = fs::File::create(dep_path).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", dep_path.display(), e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                )]
            })?;
            write_dep_file(
                &mut dep_out,
                target_ref,
                &input.display().to_string(),
                &deps,
                cfg.dep_phony,
            )
            .map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write dep file: {}", e),
                    crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
                )]
            })?;
        }
    }

    Ok(())
}

/// C++ 编译驱动: M9 接入点
///
/// 流程:
///   1. 预处理 C++ 源 (复用 C 预处理器; C++ 模式不处理 #include <iostream> 等)
///   2. 调 cpp 前端 lex/parse/lower → 降级到 C 字符串
///   3. 写到临时 .c 文件
///   4. 调 C 链路 (re-preprocess / parse / IR / codegen) 写 .o 或 .s
/// M30.4: AST lower 可能因 parser 容错恢复丢失尾部的 `main` 函数.
/// 检测: 若预处理文本含 `main` 定义但 lowered C 不含, 从预处理文本
/// 提取 `main` 函数体并追加到 lowered C 末尾.
fn recover_missing_main(c_src: String, pp_text: &str) -> String {
    // 检查 lowered C 是否已有 main
    let has_main_in_c = c_src.lines().any(|line| {
        let t = line.trim();
        t.contains("main") && (t.contains('(') || t.contains('{'))
            && !t.starts_with("//") && !t.starts_with("/*")
            && !t.contains("getdomainname") && !t.contains("setdomainname")
    });
    if has_main_in_c {
        return c_src;
    }
    // 检查预处理文本是否有 main 定义
    let pp_lines: Vec<&str> = pp_text.lines().collect();
    let mut main_start = None;
    for (i, line) in pp_lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("main") && t.contains('(') {
            main_start = Some(i);
            break;
        }
        // 也检测 "int\nmain (void)" 模式: 上一行是 "int", 当前行是 "main ("
        if t.starts_with("main") && t.contains('(') == false {
            // 检查下一行是否继续 main 的签名
            if i + 1 < pp_lines.len() && pp_lines[i + 1].trim().starts_with('(') {
                main_start = Some(i);
                break;
            }
        }
        // 检查 "int\nmain" 模式
        if t == "int" && i + 1 < pp_lines.len() {
            let next = pp_lines[i + 1].trim();
            if next.starts_with("main") {
                main_start = Some(i);
                break;
            }
        }
    }
    let main_start = match main_start {
        Some(s) => s,
        None => return c_src,
    };
    // 从 main_start 提取到匹配的 '}' 结束
    let mut brace_depth = 0i32;
    let mut found_open = false;
    let mut main_end = main_start;
    for i in main_start..pp_lines.len() {
        for ch in pp_lines[i].chars() {
            if ch == '{' { brace_depth += 1; found_open = true; }
            if ch == '}' { brace_depth -= 1; }
        }
        if found_open && brace_depth <= 0 {
            main_end = i;
            break;
        }
    }
    if !found_open {
        return c_src;
    }
    // 提取 main 函数文本
    let main_text: String = pp_lines[main_start..=main_end]
        .iter()
        .copied()
        .collect::<Vec<&str>>()
        .join("\n");
    // 追加到 lowered C 末尾
    let mut result = c_src;
    result.push_str("\n/* recovered main function (lost during AST lower) */\n");
    result.push_str(&main_text);
    result.push('\n');
    result
}

///
/// 设计: C++ 不另起 IR, 而是降级到 C 后复用整条 C 链路 (M7 决策)。
/// 这意味着 C++ 的所有限制 = 现有 C 后端能表达的限制。
fn run_cpp_driver(cfg: &Config, input: &std::path::Path) -> Result<(), Vec<Diagnostic>> {
    use crate::cpp::lower::Lower;
    use crate::cpp::parser::CppParser;

    let file_name = input.display().to_string();

    // -E 模式: 复用 C 预处理器 (C++ 注释 + 基本宏支持, 不解析 #include <iostream> 等)
    if cfg.preprocess_only {
        let src = fs::read(input).map_err(|e| {
            vec![Diagnostic::err(
                "E0100",
                format!("cannot read {}: {}", file_name, e),
                crate::diagnostics::Span::point(&file_name, 0, 0),
            )]
        })?;
        let pp_out = preproc::preprocess(
            &src,
            &file_name,
            &cfg.include_paths,
            &cfg.system_paths,
            &cfg.quote_paths,
            &cfg.after_paths,
            &cfg.defines,
            cfg.std,
            &cfg.force_includes,
            cfg.freestanding,
            &cfg.pch_opts,
            cfg.kernel_code_model,
            cfg.short_wchar,
            &cfg.target,
            &cfg.macro_prefix_maps,
            cfg.macro_backtrace_limit,
            cfg.trigraphs,
            cfg.warn_undef,
            cfg.opt,
        );
        let mut pp_diags = pp_out.diags;
        enrich_diags(&mut pp_diags, &src, &file_name, input.parent());
        if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
            return Err(pp_diags);
        }
        emit_preproc_warnings(&pp_diags);
        // -o 显式指定时写文件, 否则写 stdout (-o - 走 stdout)
        if cfg.output_explicit && cfg.output.to_str() != Some("-") {
            let path = &cfg.output;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            let mut f = fs::File::create(path).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write {}: {}", path.display(), e),
                    crate::diagnostics::Span::point(&path.display().to_string(), 0, 0),
                )]
            })?;
            f.write_all(pp_out.text.as_bytes()).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("write error: {}", e),
                    crate::diagnostics::Span::point("", 0, 0),
                )]
            })?;
        } else {
            print!("{}", pp_out.text);
        }
        return Ok(());
    }

    if cfg.verbose {
        eprintln!("dcc: cpp     = {}", input.display());
        eprintln!("dcc: target  = {}", cfg.target.as_str());
        eprintln!("dcc: std     = {}", cfg.std.as_str());
        eprintln!("dcc: -O{}", cfg.opt);
    }

    // 1. 预处理 (复用 C 预处理器, 默认追加 stdlib headers 路径)
    let src = fs::read(input).map_err(|e| {
        vec![Diagnostic::err(
            "E0100",
            format!("cannot read {}: {}", file_name, e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;
    let mut cpp_includes: Vec<PathBuf> = cfg.include_paths.iter().map(PathBuf::from).collect();
    if let Some(stdlib_inc) = crate::cpp::driver::default_stdlib_include() {
        cpp_includes.push(stdlib_inc);
    }
    let pp_out = preproc::preprocess(
        &src,
        &file_name,
        &cpp_includes,
        &cfg.system_paths,
        &cfg.quote_paths,
        &cfg.after_paths,
        &cfg.defines,
        cfg.std,
        &cfg.force_includes,
        cfg.freestanding,
        &cfg.pch_opts,
        cfg.kernel_code_model,
        cfg.short_wchar,
        &cfg.target,
        &cfg.macro_prefix_maps,
        cfg.macro_backtrace_limit,
        cfg.trigraphs,
        cfg.warn_undef,
        cfg.opt,
    );
    let mut pp_diags = pp_out.diags;
    enrich_diags(&mut pp_diags, &src, &file_name, input.parent());
    if pp_diags.iter().any(|d| matches!(d.sev, Severity::Error)) {
        return Err(pp_diags);
    }
    emit_preproc_warnings(&pp_diags);

    // Generate dependency file for C++ source using original file path
    let include_deps = pp_out.deps.clone();
    if cfg.gen_dep && !cfg.dep_no_compile {
        let dep_path = cfg.dep_file.as_ref().unwrap_or(&cfg.output);
        let target = cfg
            .dep_target
            .as_deref()
            .unwrap_or_else(|| cfg.output.to_str().unwrap_or("unknown"));
        let target_owned;
        let target_ref = if cfg.dep_quote_target {
            target_owned = quote_make_target(target);
            &target_owned
        } else {
            target
        };
        let deps: Vec<String> = if cfg.dep_mmd_only {
            filter_user_deps(include_deps.clone(), &cfg.system_paths)
        } else {
            include_deps.clone()
        };
        if let Some(parent) = dep_path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let mut dep_out = fs::File::create(dep_path).map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("cannot write {}: {}", dep_path.display(), e),
                crate::diagnostics::Span::point(&dep_path.display().to_string(), 0, 0),
            )]
        })?;
        write_dep_file(
            &mut dep_out,
            target_ref,
            &input.display().to_string(),
            &deps,
            cfg.dep_phony,
        )
        .map_err(|e| {
            vec![Diagnostic::err(
                "E0101",
                format!("cannot write dep file: {}", e),
                crate::diagnostics::Span::point("", 0, 0),
            )]
        })?;
    }

    // 2. C++ lex + parse + lower
    //    当 CppParser 失败或有错误时, 回退到“预处理 + strip C++ 关键字 → C 后端”
    //    直通模式. 此外, 当源文件本身不含 C++ 关键字 (class/template/namespace)
    //    时也使用直通模式, 以避免系统头文件中的类型定义在 lower 过程中丢失.
    //    M30.1: 检测范围加入预处理后文本 - 纯 include 型源 (`#include
    //    <type_traits>` + 使用) 的 C++ 构造全在头文件里, 只看原始源码
    //    会误走 strip 直通, 模板 / namespace 全部丢失.
    //    M30.2: 补充 `::` 作用域运算符与 new/delete/operator/访问说明符
    //    检测. `struct`/`bool` 兼容 C 的源 (libcpp/line-map.cc 风格: 结构体
    //    + 类外成员定义 `T C::m() const {}`) 此前漏网, strip 管道只会把
    //    `::` 拼成 `_` 而不剩 const / 注入 this, 产出非法 C.
    let src_has_cpp = {
        let raw = String::from_utf8_lossy(&src);
        let has = |t: &str| crate::cpp::lower::has_cpp_feature(t);
        has(&raw) || has(&pp_out.text)
    };
    let mut parser = CppParser::new(pp_out.text.as_bytes(), &file_name);
    let (decls, c_src, module_bmf) = match parser.parse_top_level() {
        Ok(ref decls) if src_has_cpp => {
            // 基于成功率的降级决策:
            // parser 容错恢复会将无法解析的声明保留为 Opaque 原文,
            // 因此即使错误数较多, 只要大部分声明解析成功,
            // AST lower + opaque 混合路径的语义保真度仍远高于
            // strip-keywords 文本处理.
            let error_count = parser.diags.iter()
                .filter(|d| matches!(d.sev, Severity::Error))
                .count();
            let total_attempted = decls.len() + error_count;
            let success_ratio = if total_attempted > 0 {
                decls.len() as f64 / total_attempted as f64
            } else {
                0.0
            };
            // 无错误时直接走 AST; 有错误时按成功率决策:
            // >= 20% 声明解析成功 → AST lower + opaque 混合路径
            // < 20% → 回退 strip-keywords
            // 阈值从 50% 降至 20%: 复杂标准库头文件 (<expected>, <print>)
            // 含大量模板声明会拖低成功率, 但 AST lower + opaque 混合路径
            // 的语义保真度仍远高于 strip-keywords.
            let use_ast = error_count == 0 || success_ratio >= 0.2;
            if use_ast {
                if error_count > 0 && cfg.verbose {
                    eprintln!(
                        "dcc: C++ parser had {} errors ({}/{} decls ok, {:.0}% success), using AST lower + opaque",
                        error_count, decls.len(), total_attempted, success_ratio * 100.0
                    );
                }
            let mut search_dirs: Vec<PathBuf> = Vec::new();
            if let Some(p) = input.parent() {
                search_dirs.push(p.to_path_buf());
            }
            if let Ok(cwd) = std::env::current_dir() {
                if !search_dirs.contains(&cwd) {
                    search_dirs.push(cwd);
                }
            }
            search_dirs.extend(cfg.include_paths.iter().cloned());
            search_dirs.extend(cfg.system_paths.iter().cloned());
            let imported = crate::cpp::modules::load_imported_modules(&decls, &search_dirs);
            let mut lower = Lower::new();
            lower.imported_modules = imported;
            lower.lower_decls(&decls);
            if let Some(msg) = lower.odr_check() {
                return Err(vec![Diagnostic::err(
                    "E0103",
                    msg,
                    crate::diagnostics::Span::point(&file_name, 0, 0),
                )]);
            }
            let module_bmf = lower.module_bmf();
            (Some(decls.clone()), lower.finish(), module_bmf)
            } else {
                // 成功率太低 (< 50%), 回退到 strip-keywords
                if cfg.verbose {
                    eprintln!(
                        "dcc: C++ parser success ratio {:.0}% < 50%, falling back to strip-keywords",
                        success_ratio * 100.0
                    );
                }
                if std::env::var("DCC_CPP_DUMP_DIAGS").is_ok() {
                    for d in &parser.diags {
                        eprintln!("dcc-cpp-diag: {:?} {} @ {:?}", d.sev, d.msg, d.span);
                    }
                }
                let stripped = crate::cpp::lower::strip_cpp_keywords(&pp_out.text);
                (None, stripped, None)
            }
        }
        Err(_) | Ok(_) => {
            // Fallback: C++ parser 失败或解析中有错误,
            // 预处理文本 → strip C++ 关键字 → 直接交 C 后端
            if cfg.verbose {
                eprintln!(
                    "dcc: C++ parser had errors, falling back to strip-keywords mode"
                );
            }
            // M30: 诊断转储 (DCC_CPP_DUMP_DIAGS 开启时打印 C++ parser
            // 降级原因, 定位为何未走 AST lower 路径)
            if std::env::var("DCC_CPP_DUMP_DIAGS").is_ok() {
                for d in &parser.diags {
                    eprintln!("dcc-cpp-diag: {:?} {} @ {:?}", d.sev, d.msg, d.span);
                }
            }
            let stripped = crate::cpp::lower::strip_cpp_keywords(&pp_out.text);
            (None, stripped, None)
        }
    };

    // Post-process: flatten any remaining :: scope operators (both AST and fallback paths)
    let c_src = crate::cpp::lower::flatten_all_scope_operators(&c_src);
    // Apply additional C++ to C conversions that might have been missed by AST lowering
    let c_src = crate::cpp::lower::apply_post_lowering_conversions(&c_src);
    // M30.4: AST lower 可能因 parser 容错恢复丢失尾部函数 (如 main).
    // 检测: 若预处理文本含 main 但 lowered C 不含, 从预处理文本提取并追加.
    let c_src = recover_missing_main(c_src, &pp_out.text);

    // -dump-ast: 打印 C++ AST 后退出
    if cfg.dump_ast {
        if let Some(ref d) = decls {
            println!("{:#?}", d);
        } else {
            eprintln!("dcc: no AST available (fallback mode)");
        }
        return Ok(());
    }

    // -fsyntax-only: 只跑 lower, 不调 dcc 编译
    if cfg.syntax_only {
        return Ok(());
    }

    // M26.4: 写出模块接口文件 (BMIP 缓存: 供后续 TU import)
    if let Some(bmf_text) = module_bmf {
        if let Some(out_path) = &cfg.emit_module_file {
            if let Some(parent) = out_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            fs::write(out_path, bmf_text).map_err(|e| {
                vec![Diagnostic::err(
                    "E0101",
                    format!("cannot write module file {}: {}", out_path.display(), e),
                    crate::diagnostics::Span::point("", 0, 0),
                )]
            })?;
            if cfg.verbose {
                eprintln!("dcc: wrote {}", out_path.display());
            }
        }
    }

    // 3. 写临时 .c
    let tmp = if let Ok(p) = std::env::var("DCC_CPP_KEEP_TMP") {
        std::path::PathBuf::from(p)
    } else {
        std::env::temp_dir().join(format!(
            "dcc-cpp-{}-{}.c",
            std::process::id(),
            input
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("input")
        ))
    };
    fs::write(&tmp, c_src).map_err(|e| {
        vec![Diagnostic::err(
            "E0101",
            format!("cannot write tmp: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;

    // 4. 调 dcc C 链路编译 (走相同 dcc 二进制, 复用 codegen/IR)
    let dcc_bin = std::env::current_exe().map_err(|e| {
        vec![Diagnostic::err(
            "E0102",
            format!("cannot find dcc: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;

    // 判断是否需要链接为可执行文件
    let output_str = cfg.output.to_string_lossy();
    let needs_link = !output_str.ends_with(".o")
        && !output_str.ends_with(".s")
        && !cfg.emit_asm
        && !cfg.relocatable;

    // 如果需要链接, 先编译到临时 .o, 再用 host cc 链接
    let (compile_output, tmp_obj) = if needs_link {
        let tmp = std::env::temp_dir().join(format!(
            "dcc-cpp-link-{}-{}.o",
            std::process::id(),
            input.file_name().and_then(|n| n.to_str()).unwrap_or("input")
        ));
        (tmp.clone(), Some(tmp))
    } else {
        (cfg.output.clone(), None)
    };

    let mut cmd = Command::new(&dcc_bin);
    // Remove DCC_CPP_KEEP_TMP so the second invocation (C compilation) doesn't overwrite the file
    cmd.env_remove("DCC_CPP_KEEP_TMP");
    cmd.arg("-c");
    cmd.arg("-fpermissive");
    cmd.arg(format!("-O{}", cfg.opt));
    cmd.arg(format!("-std={}", cfg.std.as_str()));
    for inc in &cfg.include_paths {
        cmd.arg("-I").arg(inc);
    }
    for sys in &cfg.system_paths {
        cmd.arg("-isystem").arg(sys);
    }
    for d in &cfg.defines {
        if let Some(v) = &d.1 {
            cmd.arg("-D").arg(format!("{}={}", d.0, v));
        } else {
            cmd.arg("-D").arg(&d.0);
        }
    }
    if cfg.debug_level > 0 {
        cmd.arg(format!("-g{}", cfg.debug_level));
    }
    if cfg.emit_asm {
        cmd.arg("-S");
    }
    // Dependency files are now generated by the C++ driver itself,
    // not passed to the subprocess (which would use the temp C file)
    cmd.arg("-o").arg(&compile_output);
    cmd.arg(&tmp);

    if cfg.verbose {
        eprintln!(
            "dcc: dcc -c -fpermissive -O{} -o {} {}",
            cfg.opt,
            compile_output.display(),
            tmp.display()
        );
    }

    let out = cmd.output().map_err(|e| {
        vec![Diagnostic::err(
            "E0102",
            format!("cannot spawn dcc: {}", e),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]
    })?;
    if std::env::var("DCC_CPP_KEEP_TMP").is_err() {
        let _ = fs::remove_file(&tmp);
    }
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if let Some(ref tmp_obj) = tmp_obj {
            let _ = fs::remove_file(tmp_obj);
        }
        return Err(vec![Diagnostic::err(
            "E0301",
            format!("dcc C backend failed for {}: {}", file_name, stderr),
            crate::diagnostics::Span::point(&file_name, 0, 0),
        )]);
    }

    // 5. 如果需要链接, 用 host cc 链接临时 .o 到最终可执行文件
    if let Some(ref tmp_obj) = tmp_obj {
        let cc = match std::env::var("CC") {
            Ok(v)
                if v != std::env::current_exe()
                    .ok()
                    .and_then(|p| p.to_str().map(String::from))
                    .unwrap_or_default() =>
            {
                v
            }
            _ => "cc".to_string(),
        };
        let mut link_cmd = Command::new(&cc);
        link_cmd.arg(tmp_obj).arg("-o").arg(&cfg.output);
        // 自动包含 dcc C++ 运行时库
        let dcc_runtime = std::path::PathBuf::from(
            "/root/dcc/src/cpp/stdlib/runtime/dccpp_runtime.c"
        );
        if dcc_runtime.exists() {
            link_cmd.arg(&dcc_runtime);
        }
        if cfg.verbose {
            eprintln!("dcc: host-link: {} -> {}", tmp_obj.display(), cfg.output.display());
        }
        let link_out = link_cmd.output().map_err(|e| {
            let _ = fs::remove_file(tmp_obj);
            vec![Diagnostic::err(
                "E0202",
                format!("cannot invoke {}: {}", cc, e),
                crate::diagnostics::Span::point(&file_name, 0, 0),
            )]
        })?;
        let _ = fs::remove_file(tmp_obj);
        if !link_out.status.success() {
            let stderr = String::from_utf8_lossy(&link_out.stderr);
            return Err(vec![Diagnostic::err(
                "E0202",
                format!("{} link failed: {}", cc, stderr.trim_end()),
                crate::diagnostics::Span::point(&file_name, 0, 0),
            )]);
        }
    }

    eprintln!("dcc: wrote {}", cfg.output.display());
    Ok(())
}

/// 内置全链接器产生的临时 .o 清理 guard: 无论编译子任务、链接还是写输出
/// 哪条路径提前返回, 栈展开时都会删除这些临时文件 (RAII, 等价 finally)。
struct TmpObjCleanup(Vec<PathBuf>);

impl Drop for TmpObjCleanup {
    fn drop(&mut self) {
        for t in &self.0 {
            let _ = std::fs::remove_file(t);
        }
    }
}

/// 将 cli::Arch 映射到 debug::ArchKind
fn arch_from_target(t: &crate::cli::TargetTriple) -> crate::debug::ArchKind {
    use crate::debug::ArchKind;
    match t.arch {
        crate::cli::Arch::X86_64 => ArchKind::X86_64,
        crate::cli::Arch::AArch64 => ArchKind::AArch64,
        crate::cli::Arch::RiscV64 => ArchKind::RiscV64,
        crate::cli::Arch::RiscV32 => ArchKind::RiscV32,
        crate::cli::Arch::Mips64 => ArchKind::Mips64,
        crate::cli::Arch::Mips => ArchKind::Mips,
        crate::cli::Arch::Arm => ArchKind::Arm,
    }
}

/// 从 raw_args 解析 -Wl,--dynamic-linker= 或 -Wl,-dynamic-linker, 选项。
/// 支持格式:
///   -Wl,--dynamic-linker=/path
///   -Wl,--dynamic-linker,/path
///   -Wl,-dynamic-linker,/path
fn parse_dynamic_linker(raw_args: &[String]) -> Option<String> {
    for arg in raw_args {
        if let Some(path) = arg.strip_prefix("-Wl,--dynamic-linker=") {
            return Some(path.to_string());
        }
        if let Some(rest) = arg.strip_prefix("-Wl,--dynamic-linker,") {
            return Some(rest.to_string());
        }
        if let Some(rest) = arg.strip_prefix("-Wl,-dynamic-linker,") {
            return Some(rest.to_string());
        }
    }
    None
}

/// 返回架构对应的动态链接器路径
fn dynamic_linker_path(arch: crate::debug::ArchKind) -> String {
    match arch {
        crate::debug::ArchKind::X86_64 => "/lib64/ld-linux-x86-64.so.2",
        crate::debug::ArchKind::AArch64 => "/lib/ld-linux-aarch64.so.1",
        crate::debug::ArchKind::RiscV64 => "/lib/ld-linux-riscv64-lp64d.so.1",
        crate::debug::ArchKind::RiscV32 => "", // bare-metal, no dynamic linker
        crate::debug::ArchKind::Mips64 => "/lib/ld-linux-mipsn8.so.1",
        crate::debug::ArchKind::Mips => "/lib/ld-linux-mipsel.so.1",
        crate::debug::ArchKind::Arm => "/lib/ld-linux-armhf.so.3",
    }
    .to_string()
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
    // 源文件输入 (.c/.i/.S 等): 先编译为临时 .o 再交给内置链接器,
    // 否则链接器会把 C 源码当 ELF 解析 (bad ELF magic)。
    // Multiple source files are compiled in parallel when jobs != Some(1).
    let mut tmp_guard = TmpObjCleanup(Vec::new());
    let mut inputs: Vec<std::path::PathBuf> = Vec::new();

    // Collect source files needing compilation with their original indices
    let source_tasks: Vec<(usize, &std::path::PathBuf)> = cfg
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.extension().map(|e| e == "o").unwrap_or(false))
        .collect();
    let n_sources = source_tasks.len();
    let force_single = cfg.jobs == Some(1);
    let parallel_sources = n_sources >= 2 && !force_single;

    if parallel_sources {
        // Pre-allocate temp .o paths and cleanup guard
        let tmp_objs: Vec<PathBuf> = source_tasks
            .iter()
            .map(|(i, _)| {
                std::env::temp_dir().join(format!("dcc-builtlink-{}-{}.o", std::process::id(), i))
            })
            .collect();
        for tmp_o in &tmp_objs {
            tmp_guard.0.push(tmp_o.clone());
        }

        // Parallel source compilation using std::thread::scope
        let degree = cfg
            .jobs
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|p| p.get())
                    .unwrap_or(4)
            })
            .min(n_sources);
        if cfg.verbose {
            eprintln!(
                "dcc: built-in linker: compiling {} sources with {} worker(s)",
                n_sources, degree
            );
        }
        let next_job = std::sync::atomic::AtomicUsize::new(0);
        let results = std::sync::Mutex::new(vec![None; n_sources]);
        std::thread::scope(|scope| {
            for _ in 0..degree {
                scope.spawn(|| loop {
                    let j = next_job.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if j >= n_sources {
                        break;
                    }
                    let (_, _inp) = source_tasks[j];
                    let mut c = cfg.clone();
                    c.output = tmp_objs[j].clone();
                    c.relocatable = true;
                    c.built_link = false;
                    results.lock().unwrap()[j] = Some(run(c));
                });
            }
        });
        // Process results in order; first error aborts (matches prior
        // sequential `run(c)?` behavior)
        for result in results
            .into_inner()
            .expect("built-link parallel compile mutex poisoned")
        {
            result.expect("built-link parallel compile dropped a file")?;
        }
        // Merge .o inputs and temp .o paths in original order
        let mut src_idx = 0;
        for inp in cfg.inputs.iter() {
            let is_obj = inp.extension().map(|e| e == "o").unwrap_or(false);
            if is_obj {
                inputs.push(inp.clone());
            } else {
                inputs.push(tmp_objs[src_idx].clone());
                src_idx += 1;
            }
        }
    } else {
        // Sequential fallback: single source or -j1
        for (i, inp) in cfg.inputs.iter().enumerate() {
            let is_obj = inp.extension().map(|e| e == "o").unwrap_or(false);
            if is_obj {
                inputs.push(inp.clone());
            } else {
                let tmp_o = std::env::temp_dir().join(format!(
                    "dcc-builtlink-{}-{}.o",
                    std::process::id(),
                    i
                ));
                let mut c = cfg.clone();
                c.output = tmp_o.clone();
                c.relocatable = true;
                c.built_link = false;
                tmp_guard.0.push(tmp_o.clone());
                run(c)?;
                inputs.push(tmp_o);
            }
        }
    }
    let output_kind = match cfg.output_kind {
        LinkerOutputKind::Executable => link::OutputKind::Executable,
        LinkerOutputKind::PIE => link::OutputKind::PIE,
        LinkerOutputKind::Shared => link::OutputKind::Shared,
    };
    let mut needed_libs: Vec<String> = cfg
        .link_libs
        .iter()
        .map(|s| format!("lib{}.so", s))
        .collect();
    let mut allowed_undef = vec![
        "memcpy".into(),
        "memmove".into(),
        "memset".into(),
        "memcmp".into(),
        "printf".into(),
        "puts".into(),
        "exit".into(),
        "abort".into(),
        "malloc".into(),
        "free".into(),
        "__stack_chk_fail".into(),
        "__cxa_atexit".into(),
        "__cxa_finalize".into(),
        "_start".into(),
        "main".into(),
    ];
    push_sanitizer_link_deps(cfg, &mut needed_libs, &mut allowed_undef);
    let mut link_cfg = link::LinkConfig {
        output_kind,
        lto: cfg.linker_lto,
        entry: cfg.linker_entry.clone().unwrap_or_else(|| {
            if matches!(output_kind, link::OutputKind::PIE) {
                "_start".into()
            } else if matches!(output_kind, link::OutputKind::Shared) {
                String::new()
            } else {
                "main".into()
            }
        }),
        base_addr: 0x400000,
        soname: cfg.soname.clone(),
        needed: needed_libs,
        strip_debug: false,
        gc_sections: true,
        allowed_undef,
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
        // target_clones: 内置链接器从目标文件的 IFUNC 符号自动检测,
        // 无需从 AST 显式注册 (codegen 已生成 resolver 和 STT_GNU_IFUNC 符号)
        target_clones: link::target_clones::TargetClonesTable::new(),
        fini_builder: link::fini_dispatch::FiniArrayBuilder::new(),
        arch: arch_from_target(&cfg.target),
        // PIE/.so 需要 PT_INTERP 让动态链接器处理 GOT/重定位;
        // 支持 -Wl,--dynamic-linker= 覆盖默认路径
        dynamic_linker: parse_dynamic_linker(&cfg.raw_args)
            .or_else(|| {
                matches!(
                    output_kind,
                    link::OutputKind::PIE | link::OutputKind::Shared
                )
                .then(|| dynamic_linker_path(arch_from_target(&cfg.target)))
            }),
        vdso: cfg.vdso.clone().or_else(|| {
            // 自动检测系统 vDSO (仅 Executable/PIE 需要)
            if matches!(output_kind, link::OutputKind::Executable | link::OutputKind::PIE) {
                link::vdso::detect_system_vdso(arch_from_target(&cfg.target))
            } else {
                None
            }
        }),
        dso_cache: link::dso_cache::DsoCache::new(),
        lib_search_paths: cfg.lib_search_paths.clone(),
        linker_script: cfg.linker_script.clone(),
        output_format: if cfg.target.os == crate::cli::Os::Windows {
            link::OutputFormat::Pe
        } else {
            link::OutputFormat::Elf
        },
        whole_archive_libs: cfg.whole_archive_libs.clone(),
        dead_symbols: vec![],
    };
    // 解析 .symver 指令: 从 raw_args 提取 __asm__(".symver ...") 模式
    // 实际项目中由 codegen 阶段把 .symver 写入 .symver 节, 此处给个兜底:
    for arg in &cfg.raw_args {
        if let Some(rest) = arg.strip_prefix(".symver=") {
            link_cfg
                .version_table
                .add_symver(&format!(".symver {}", rest));
        }
    }
    if cfg.linker_entry.is_some() {
        link_cfg.entry = cfg.linker_entry.clone().unwrap();
    }
    // P9: 自动检测系统 libc 并将其导出符号加入 allowed_undef,
    // 减少内置链接器对 host cc 的依赖。
    if matches!(
        output_kind,
        link::OutputKind::Executable | link::OutputKind::PIE
    ) {
        if let Ok(libc_info) = link::dso_cache::resolve_and_load(
            &mut link_cfg.dso_cache,
            "c",
            &link_cfg.lib_search_paths,
            None,
        ) {
            let extra: Vec<String> = libc_info
                .dyn_symbols
                .keys()
                .filter(|s| !link_cfg.allowed_undef.contains(s))
                .cloned()
                .collect();
            if cfg.verbose && !extra.is_empty() {
                eprintln!(
                    "dcc-link: auto-resolved {} libc symbols from {}",
                    extra.len(),
                    libc_info.path.display()
                );
            }
            link_cfg.allowed_undef.extend(extra);
        }
    }
    let bytes = if cfg.linker_lto {
        // LTO path: read .dcc.lto sections from input .o files,
        // deserialize bitcode, run cross-TU optimization, then link
        if cfg.verbose {
            eprintln!(
                "dcc: LTO enabled, extracting bitcode from {} inputs",
                inputs.len()
            );
        }
        let object_bytes: Vec<Vec<u8>> = inputs
            .iter()
            .filter_map(|p| std::fs::read(p).ok())
            .collect();

        let mut bc_modules: Vec<link::lto_bitcode::BitcodeModule> = object_bytes
            .iter()
            .filter_map(|obj| {
                if obj.is_empty() {
                    return None;
                }
                link::lto_bitcode::deserialize_module(
                    &crate::codegen::elf::read_section_data(obj, ".dcc.lto").unwrap_or_default(),
                )
                .ok()
            })
            .collect();

        if cfg.verbose {
            eprintln!("dcc: extracted {} bitcode modules", bc_modules.len());
        }

        if !bc_modules.is_empty() {
            // Run cross-TU optimization passes
            let dead_fns = link::lto_bitcode::run_cross_tu_lto(&mut bc_modules);
            if cfg.verbose && !dead_fns.is_empty() {
                eprintln!(
                    "dcc: cross-TU LTO optimization complete, {} dead function(s)",
                    dead_fns.len()
                );
            }
            // P7: 将死函数符号传递给链接器，在最终 ELF 中剥离
            link_cfg.dead_symbols = dead_fns;
        }

        // Link with optimized bitcode (currently just calls link_multi_tu)
        // Future: re-codegen inlined functions before linking
        link::link_multi_tu(&inputs, &cfg.output, &link_cfg)?
    } else {
        link::link_multi_tu(&inputs, &cfg.output, &link_cfg)?
    };
    // tmp_guard drop 时统一清理临时 .o (链接/写输出失败路径也不例外)
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
    eprintln!(
        "dcc: built-in linker wrote {} ({} bytes, {:?})",
        cfg.output.display(),
        bytes.len(),
        output_kind
    );
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
    if cfg.link_mode {
        return false;
    } // -r
    if cfg.built_link {
        return false;
    } // -pie / -shared / -fbuiltin-linker
    if cfg.relocatable {
        return false;
    } // -c
    if cfg.emit_asm {
        return false;
    } // -S
    if cfg.preprocess_only {
        return false;
    } // -E
    if cfg.dep_no_compile {
        return false;
    } // -M/-MM
    if cfg.syntax_only {
        return false;
    } // -fsyntax-only
    if cfg.dump_ast {
        return false;
    } // -dump-ast
    let out = cfg.output.to_string_lossy();
    if out.ends_with(".o") || out.ends_with(".s") || out.ends_with(".a") || out.ends_with(".so") {
        return false;
    }
    // 多输入且全部是 .o/.a: 纯链接任务, 委托 host cc
    if cfg.inputs.len() != 1 && all_inputs_are_objects(&cfg.inputs) {
        return true;
    }
    if cfg.inputs.len() != 1 {
        return false;
    }
    true
}

/// 检查所有输入是否都是目标文件或归档 (.o / .obj / .a)
fn all_inputs_are_objects(inputs: &[std::path::PathBuf]) -> bool {
    inputs.iter().all(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e, "o" | "obj" | "a"))
            .unwrap_or(false)
    })
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
    let whole_archive_libs = cfg.whole_archive_libs.clone();
    let sysroot = cfg.sysroot.clone();
    let verbose = cfg.verbose;

    // 纯链接模式: 所有输入都是 .o/.a, 无需编译, 直接委托 host cc 链接
    if all_inputs_are_objects(&cfg.inputs) {
        let input_strs: Vec<String> = cfg.inputs.iter().map(|p| p.display().to_string()).collect();
        if verbose {
            eprintln!(
                "dcc: host-link (pure): {} -> {}",
                input_strs.join(", "),
                final_output.display()
            );
        }
        let link_result = link_objects_with_host_cc(
            &cfg.inputs,
            &final_output,
            &link_libs,
            &whole_archive_libs,
            sysroot.as_deref(),
            verbose,
        );
        if link_result.is_ok() {
            eprintln!("dcc: {}", final_output.display());
        }
        return link_result;
    }

    let input_name = cfg
        .inputs
        .first()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    let tmp_o = std::env::temp_dir().join(format!(
        "dcc-hostlink-{}-{}.o",
        std::process::id(),
        input_name,
    ));
    if verbose {
        eprintln!(
            "dcc: host-link: {} -> {} -> {}",
            cfg.inputs[0].display(),
            tmp_o.display(),
            final_output.display()
        );
    }
    // 1) 临时 .o：设 relocatable=true 后递归 run()
    cfg.output = tmp_o.clone();
    cfg.relocatable = true;
    let sanitizer_flags = host_sanitizer_flags(&cfg);
    // 保存 cfg 副本, 以便后续 IFUNC 检测后可能走内置链接器
    let cfg_saved = cfg.clone();
    // 强制走内置链接器 ≠ 1 路径是上面"递归后"需走的；现在 cfg.built_link
    // 仍是 false，所以 .o 由 run() 末端 codegen 直接输出。
    let result = run(cfg);
    if result.is_err() {
        let _ = fs::remove_file(&tmp_o);
        return result;
    }
    // 2) 检测 .o 中是否包含 IFUNC 符号 (target_clones / __attribute__((ifunc)))
    //    宿主 cc/ld 无法处理 dcc 生成的 IFUNC resolver (resolver 代码在
    //    内置链接器中生成), 此时必须走内置链接器路径.
    let has_ifunc = fs::read(&tmp_o)
        .ok()
        .map(|bytes| elf_has_ifunc_symbols(&bytes))
        .unwrap_or(false);
    if has_ifunc {
        if verbose {
            eprintln!(
                "dcc: IFUNC symbols detected in {}, using builtin linker",
                tmp_o.display()
            );
        }
        let mut link_cfg = cfg_saved.clone();
        link_cfg.inputs = vec![tmp_o.clone()];
        link_cfg.output = final_output.clone();
        link_cfg.built_link = true;
        link_cfg.relocatable = false;
        let link_result = run_builtin_linker(&link_cfg);
        let _ = fs::remove_file(&tmp_o);
        if link_result.is_ok() {
            eprintln!("dcc: {} (via builtin linker, IFUNC)", final_output.display());
        }
        return link_result;
    }
    // 3) 无 IFUNC: 调 host cc 链接
    let link_result = link_with_host_cc(
        &tmp_o,
        &final_output,
        &link_libs,
        &whole_archive_libs,
        sysroot.as_deref(),
        verbose,
        &sanitizer_flags,
    );
    let _ = fs::remove_file(&tmp_o);
    if link_result.is_ok() {
        eprintln!("dcc: {} (via host cc)", final_output.display());
    }
    link_result
}

/// 检测 ELF 对象文件是否包含 STT_GNU_IFUNC 类型符号.
fn elf_has_ifunc_symbols(elf: &[u8]) -> bool {
    use crate::codegen::elf::{ELFCLASS64, ELFMAG, SHT_SYMTAB};
    if elf.len() < 64 || elf.get(0..4) != Some(&ELFMAG) || elf[4] != ELFCLASS64 {
        return false;
    }
    let shoff = u64::from_le_bytes(
        elf.get(40..48)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    ) as usize;
    let shentsize = u16::from_le_bytes(
        elf.get(58..60)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shnum = u16::from_le_bytes(
        elf.get(60..62)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    if shnum == 0 || shentsize < 64 || shoff + shnum * shentsize > elf.len() {
        return false;
    }
    // 找 .symtab
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > elf.len() {
            break;
        }
        let sh_type = u32::from_le_bytes(elf[base + 4..base + 8].try_into().unwrap());
        if sh_type != SHT_SYMTAB {
            continue;
        }
        let data_off =
            u64::from_le_bytes(elf[base + 24..base + 32].try_into().unwrap()) as usize;
        let data_size =
            u64::from_le_bytes(elf[base + 32..base + 40].try_into().unwrap()) as usize;
        let entsize =
            u64::from_le_bytes(elf[base + 56..base + 64].try_into().unwrap()) as usize;
        if entsize < 24 || data_off + data_size > elf.len() {
            return false;
        }
        let sym_count = data_size / entsize;
        for j in 0..sym_count {
            let sym_base = data_off + j * entsize;
            if sym_base + 24 > elf.len() {
                break;
            }
            let st_info = elf[sym_base + 4];
            let st_type = st_info & 0xf;
            if st_type == 10 {
                // STT_GNU_IFUNC = 10
                return true;
            }
        }
    }
    false
}

fn add_unique_string(v: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    if !v.iter().any(|s| s == &value) {
        v.push(value);
    }
}

/// 用户态 sanitizer 插桩依赖宿主 sanitizer runtime。
///
/// - host cc 路径通过 `-fsanitize=*` 让 cc 选择 crt/runtime。
/// - 内置动态链接器路径通过 DT_NEEDED + allowed undef 暴露给 ld.so。
/// - `-c` / `-S` 不触发链接, 保留 relocatable undefined runtime 符号。
/// - kernel-* sanitizer 由内核运行时提供, 不链接宿主 asan/hwasan。
fn sanitizer_link_deps(cfg: &Config) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut cc_flags = Vec::new();
    let mut libs = Vec::new();
    let mut undefs = Vec::new();

    fn add_runtime_dep(
        libs: &mut Vec<String>,
        cc_flags: &mut Vec<String>,
        undefs: &mut Vec<String>,
        lib: Option<&str>,
        syms: &[&str],
        flag: Option<&str>,
    ) {
        if let Some(lib) = lib {
            add_unique_string(libs, lib);
        }
        if let Some(flag) = flag {
            add_unique_string(cc_flags, flag);
        }
        for sym in syms {
            add_unique_string(undefs, *sym);
        }
    }

    macro_rules! add_lib {
        ($lib:expr, $syms:expr, $flag:expr $(,)?) => {
            add_runtime_dep(
                &mut libs,
                &mut cc_flags,
                &mut undefs,
                Some($lib),
                $syms,
                $flag,
            )
        };
    }

    if cfg.bounds_check || cfg.sanitizers.ubsan {
        add_lib!(
            "ubsan",
            &[
                "__ubsan_handle_divrem_overflow",
                "__ubsan_handle_shift_out_of_bounds",
                "__ubsan_handle_add_overflow",
                "__ubsan_handle_sub_overflow",
                "__ubsan_handle_mul_overflow",
                "__ubsan_handle_negate_overflow",
                "__ubsan_handle_vla_bound_not_positive",
                "__ubsan_handle_type_mismatch_v1",
                "__ubsan_handle_float_cast_overflow",
            ],
            Some("-fsanitize=undefined"),
        );
    }
    if cfg.sanitizers.kasan && !cfg.sanitizers.kernel_mode {
        add_lib!(
            "asan",
            &[
                "__asan_init",
                "__asan_load1",
                "__asan_load2",
                "__asan_load4",
                "__asan_load8",
                "__asan_load16",
                "__asan_loadN",
                "__asan_store1",
                "__asan_store2",
                "__asan_store4",
                "__asan_store8",
                "__asan_store16",
                "__asan_storeN",
                "__asan_poison_memory_region",
                "__asan_unpoison_memory_region",
            ],
            Some("-fsanitize=address"),
        );
    }
    if cfg.sanitizers.hwasan && !cfg.sanitizers.kernel_mode {
        add_lib!(
            "hwasan",
            &[
                "__hwasan_init",
                "__hwasan_load1",
                "__hwasan_load2",
                "__hwasan_load4",
                "__hwasan_load8",
                "__hwasan_load16",
                "__hwasan_loadN",
                "__hwasan_store1",
                "__hwasan_store2",
                "__hwasan_store4",
                "__hwasan_store8",
                "__hwasan_store16",
                "__hwasan_storeN",
            ],
            Some("-fsanitize=hwaddress"),
        );
    }
    if cfg.sanitizers.kcsan && !cfg.sanitizers.kernel_mode {
        add_lib!(
            "tsan",
            &[
                "__tsan_init",
                "__tsan_func_entry",
                "__tsan_read1",
                "__tsan_read2",
                "__tsan_read4",
                "__tsan_read8",
                "__tsan_read16",
                "__tsan_read_range",
                "__tsan_write1",
                "__tsan_write2",
                "__tsan_write4",
                "__tsan_write8",
                "__tsan_write16",
                "__tsan_write_range",
            ],
            Some("-fsanitize=thread"),
        );
    }
    if cfg.sanitizers.kcsan && cfg.sanitizers.kernel_mode {
        add_runtime_dep(
            &mut libs,
            &mut cc_flags,
            &mut undefs,
            None,
            &["__kcsan_init", "__kcsan_func_entry", "__kcsan_check_access"],
            None,
        );
    }
    if cfg.sanitizers.fuzzer || cfg.sanitizers.kcov {
        add_lib!(
            "asan",
            &[
                "__sanitizer_cov_trace_pc",
                "__sanitizer_cov_trace_pc_guard",
                "__sanitizer_cov_trace_pc_guard_init",
                "__sanitizer_cov_trace_cmp1",
                "__sanitizer_cov_trace_cmp2",
                "__sanitizer_cov_trace_cmp4",
                "__sanitizer_cov_trace_cmp8",
                "__sanitizer_cov_trace_switch",
                "__sanitizer_cov_8bit_counters_init",
                "__sanitizer_cov_pcs_init",
            ],
            None,
        );
    }
    if cfg.sanitizers.fuzzer {
        add_unique_string(&mut cc_flags, "-fsanitize=fuzzer");
    }

    (cc_flags, libs, undefs)
}

fn push_sanitizer_link_deps(
    cfg: &Config,
    needed_libs: &mut Vec<String>,
    allowed_undef: &mut Vec<String>,
) {
    let (_, libs, undefs) = sanitizer_link_deps(cfg);
    for lib in libs {
        add_unique_string(needed_libs, format!("lib{lib}.so"));
    }
    for sym in undefs {
        add_unique_string(allowed_undef, sym);
    }
}

/// sanitizer 插桩产生的 __asan_* / __ubsan_* 调用符号由宿主运行时库提供:
/// 把对应的 `-fsanitize=*` 透传给 host cc, 否则链接报 undefined reference
/// (此前这些符号不写入 .o, 链接“成功”但 call 落到地址 0, 运行即段错误)。
/// kernel-* sanitizer 不透传宿主 asan/hwasan 运行时。
fn host_sanitizer_flags(cfg: &Config) -> Vec<String> {
    sanitizer_link_deps(cfg).0
}

/// 调 host `cc` 把单 .o 链成可执行。传递 --sysroot= 与 -l 列表。
/// 失败时返回包含 cc 自身 stdout/stderr 的诊断 E0202。
///
/// 启动选项 (与 dcc 旧内部链接器输出对齐):
///   - -no-pie : dcc 现有 codegen 采用 RIP-相对 / 静态地址访问 TLS / 全局;
///     PIE 下需要重写为 -fpie. 在 -no-pie 下生成 ET_EXEC, 所有地址
///     运行时固定, 与 dcc codegen 期望一致.
///   - -z norelro : 避免 GNU_RELRO 覆盖 .tdata, 造成 TLS 写入段错误 (因为
///     dcc 的 .tdata 位于 .data LOAD 段首, 与 RELRO 重叠).
///   - -Wl,-z,noexecstack : dcc 当前不输出 .note.GNU-stack, 避免 ld 警告
///     "missing .note.GNU-stack section implies executable stack".
fn link_with_host_cc(
    obj_path: &Path,
    output: &Path,
    link_libs: &[String],
    whole_archive_libs: &[String],
    sysroot: Option<&Path>,
    verbose: bool,
    sanitizer_flags: &[String],
) -> Result<(), Vec<Diagnostic>> {
    // CC 环境变量优先, 与 GNU make 一致; 缺省为 cc.
    // 防递归: GCC 构建中 `CC=dcc` 是常见写法, 如果这里读 CC=dcc,
    // 会导致 dcc 递归调用 dcc, 最终试图把 .o 当 C 源预处,
    // 触发 E0208 ("source is not valid UTF-8"). 遇 CC 指向自己时回退
    // 到默认 "cc" (host 系统上 99% 是 gcc), 与 gcc 手工调用 dcc 时
    // 设 `CC=gcc` 一致.
    let cc = match std::env::var("CC") {
        Ok(v)
            if v != std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from))
                .unwrap_or_default() =>
        {
            v
        }
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
        // --whole-archive 库: 包裹 -Wl,--whole-archive / --no-whole-archive
        if whole_archive_libs.contains(lib) {
            cmd.arg("-Wl,--whole-archive");
        }
        cmd.arg(format!("-l{}", lib));
        if whole_archive_libs.contains(lib) {
            cmd.arg("-Wl,--no-whole-archive");
        }
    }
    for sf in sanitizer_flags {
        cmd.arg(sf);
    }
    if verbose {
        eprintln!("dcc: invoking {}: {:?}", cc, cmd);
    }
    let out = cmd.output().map_err(|e| {
        vec![Diagnostic::err(
            "E0202",
            format!("cannot invoke {}: {} (set CC= or install gcc)", cc, e),
            crate::diagnostics::Span::point("", 0, 0),
        )]
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut msg = format!("{} link failed (exit {:?})", cc, out.status.code());
        for chunk in [stderr.trim_end(), stdout.trim_end()] {
            if !chunk.is_empty() {
                msg.push_str(": ");
                msg.push_str(chunk);
            }
        }
        return Err(vec![Diagnostic::err(
            "E0202",
            msg,
            crate::diagnostics::Span::point(&output.display().to_string(), 0, 0),
        )]);
    }
    Ok(())
}

/// 多个 .o/.a 文件纯链接: 委托 host cc 完成链接.
/// 与 link_with_host_cc 相同逻辑, 但接受多个输入文件.
fn link_objects_with_host_cc(
    objects: &[std::path::PathBuf],
    output: &Path,
    link_libs: &[String],
    whole_archive_libs: &[String],
    sysroot: Option<&Path>,
    verbose: bool,
) -> Result<(), Vec<Diagnostic>> {
    let cc = match std::env::var("CC") {
        Ok(v)
            if v != std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from))
                .unwrap_or_default() =>
        {
            v
        }
        _ => "cc".to_string(),
    };
    let mut cmd = Command::new(&cc);
    for obj in objects {
        cmd.arg(obj);
    }
    // 自动包含 dcc C++ 运行时库 (如果存在)
    let dcc_runtime = std::path::PathBuf::from(
        "/root/dcc/src/cpp/stdlib/runtime/dccpp_runtime.c"
    );
    if dcc_runtime.exists() {
        cmd.arg(&dcc_runtime);
        // dccpp_runtime.c 使用 __cxa_allocate_exception / __cxa_throw
        // 等 C++ 异常运行时函数, 需要链接 libstdc++.
        cmd.arg("-lstdc++");
    }
    cmd.arg("-o").arg(output);
    if let Some(sr) = sysroot {
        cmd.arg(format!("--sysroot={}", sr.display()));
    }
    for lib in link_libs {
        if whole_archive_libs.contains(lib) {
            cmd.arg("-Wl,--whole-archive");
        }
        cmd.arg(format!("-l{}", lib));
        if whole_archive_libs.contains(lib) {
            cmd.arg("-Wl,--no-whole-archive");
        }
    }
    if verbose {
        eprintln!("dcc: invoking {}: {:?}", cc, cmd);
    }
    let out = cmd.output().map_err(|e| {
        vec![Diagnostic::err(
            "E0202",
            format!("cannot invoke {}: {}", cc, e),
            crate::diagnostics::Span::point("", 0, 0),
        )]
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut msg = format!("{} link failed (exit {:?})", cc, out.status.code());
        for chunk in [stderr.trim_end(), stdout.trim_end()] {
            if !chunk.is_empty() {
                msg.push_str(": ");
                msg.push_str(chunk);
            }
        }
        return Err(vec![Diagnostic::err(
            "E0202",
            msg,
            crate::diagnostics::Span::point(&output.display().to_string(), 0, 0),
        )]);
    }
    Ok(())
}

/// IR/优化阶段产生的诊断: span 是预处理输出行号, 需要与 parse 阶段
/// 相同的 remap + enrich 处理, 否则内核等大 TU 会把 60 万行的物理
/// 行号直接打到 stderr (如 E1009 unknown aggregate member).
fn remap_enrich(
    mut diags: Vec<Diagnostic>,
    line_map: &[LineSpan],
    src: &[u8],
    file_name: &str,
    base_dir: Option<&std::path::Path>,
) -> Vec<Diagnostic> {
    remap_diags(&mut diags, line_map);
    enrich_diags(&mut diags, src, file_name, base_dir);
    diags
}

/// 把诊断的 span 从预处理输出行号映射回源文件 (file, line).
/// 使用预处理阶段记录的 LineSpan 区间进行转换。
fn remap_diags(diags: &mut [Diagnostic], line_map: &[LineSpan]) {
    for d in diags.iter_mut() {
        if d.span.line > 0 {
            remap_span(&mut d.span, line_map);
        }
    }
}

/// 将单个 span 从预处理文本行号映射回源文件位置
fn remap_span(span: &mut crate::diagnostics::Span, line_map: &[LineSpan]) {
    let line = span.line;
    if line_map.is_empty() {
        return;
    }
    // 二分查找最后一个 out_start <= line 的区间
    let idx = line_map.partition_point(|s| s.out_start <= line);
    if let Some(ls) = idx.checked_sub(1).and_then(|i| line_map.get(i)) {
        // 严格命中: line 落在 [out_start, out_start + count) 内
        if line < ls.out_start + ls.count {
            span.file = std::sync::Arc::from(ls.file.as_str());
            span.line = ls.src_start + (line - ls.out_start);
            return;
        }
        // 未严格命中 (parser EOF 行号刚好超出 / 宏展开 / 缓存回放)
        // 仍回填文件归属, 但保留原始行号: 让下游 enrich_diags 根据
        // 磁盘文件实际行数决定是否需要 clamp, 避免此处过度截断导致
        // 简单文件的 EOF 诊断行号回退 (e0002 回归).
        span.file = std::sync::Arc::from(ls.file.as_str());
        return;
    }
    // line 小于 line_map[0].out_start: 预处理输出起始的预写入行,
    // 尝试用第一个区间的文件 (通常是首个 -include 头).
    if let Some(first) = line_map.first() {
        span.file = std::sync::Arc::from(first.file.as_str());
        span.line = first.src_start.max(1);
    }
}

/// 把 GCOV 单元内所有块的行号从预处理输出坐标映射回源文件坐标,
/// 使 `.gcno` / 覆盖率报告指向真实源码行。
fn remap_gcov_lines(unit: &mut crate::profile::gcov::GcovUnit, line_map: &[LineSpan]) {
    for f in &mut unit.functions {
        for b in &mut f.blocks {
            for l in &mut b.lines {
                let mut sp = crate::diagnostics::Span::point(&f.file, *l, 1);
                remap_span(&mut sp, line_map);
                *l = sp.line;
            }
        }
    }
}

/// 收集 offload entries 并构造 `.dcc.offload` 节内容。
///
/// 输入:
///   * OpenMP: `#pragma omp target [simd]` / `#pragma omp simd` 指令
///     (preproc 已记录精确源文件 + 源行号); 无 `-fopenmp-targets=` 时
///     device_triple 为空串 (host fallback), 有则每个 triple 展开一条。
///   * CUDA/HIP: `__global__` / `__device__` / `__host__` 限定符函数
///     (仅 -fcuda / -fhip 时 parser 接受). `--offload-arch=` 指定时每个
///     arch 映射为 device triple 展开。
///
/// 诊断位置约定: 所有返回的诊断 span 均为源文件坐标 (已在函数内
/// 完成 remap), 调用方只需 enrich_diags 填充 source_line。entry
/// 内的 file/line/col 同样为源坐标, 直接进入元数据。
fn collect_offload_section(
    prog: &crate::ast::Program,
    offload: &crate::profile::offload::OffloadConfig,
    omp_pragmas: &[(String, u32, String)],
    line_map: &[LineSpan],
) -> (
    Option<crate::profile::offload::OffloadSection>,
    Vec<Diagnostic>,
) {
    use crate::ast::Decl;
    use crate::profile::offload::{arch_to_triple, OffloadEntry, OffloadKind, OffloadSection};

    let mut section = OffloadSection::new();
    let mut diags = Vec::new();
    if !offload.any() {
        return (None, diags);
    }

    // ---- CUDA / HIP: __global__ / __device__ / __host__ 限定符函数 ----
    if offload.cuda || offload.hip {
        let kind = if offload.hip {
            OffloadKind::Hip
        } else {
            OffloadKind::Cuda
        };
        let default_triple = if offload.hip {
            "amdgcn-amd-amdhsa"
        } else {
            "nvptx64-nvidia-cuda"
        };
        // --offload-arch 展开: arch -> (triple, arch 名)
        let archs: Vec<(String, Option<String>)> = if offload.target_archs.is_empty() {
            vec![(default_triple.to_string(), None)]
        } else {
            offload
                .target_archs
                .iter()
                .filter_map(|a| arch_to_triple(a).map(|t| (t.to_string(), Some(a.clone()))))
                .collect()
        };
        for decl in &prog.decls {
            let f = match decl {
                Decl::Func(f) => f,
                _ => continue,
            };
            if !(f.is_global || f.is_device || f.is_host) {
                continue;
            }
            // 语义校验: __global__ 不能与 __host__/__device__ 混用
            if f.is_global && (f.is_host || f.is_device) {
                let mut err_sp = f.span.clone();
                remap_span(&mut err_sp, line_map);
                diags.push(Diagnostic::err(
                    "E0310",
                    format!(
                        "a __global__ function '{}' cannot also be __host__ or __device__",
                        f.name.name
                    ),
                    err_sp,
                ));
                continue;
            }
            // 纯 __host__ 函数不是 offload entry (仅 host 侧可见)
            if !(f.is_global || f.is_device) {
                continue;
            }
            let mut sp = f.span.clone();
            remap_span(&mut sp, line_map);
            for (triple, arch) in &archs {
                section.add_entry(OffloadEntry {
                    kind,
                    func: f.name.name.clone(),
                    file: sp.file.to_string(),
                    line: sp.line,
                    col: sp.col,
                    device_triple: triple.clone(),
                    arch: arch.clone(),
                });
            }
        }
    }

    // ---- OpenMP: #pragma omp target [simd] / simd ----
    if offload.openmp || offload.openmp_simd {
        // 有 body 的函数区间表: (函数名, 源文件, 起始行, 结束行)
        let funcs: Vec<(String, String, u32, u32)> = prog
            .decls
            .iter()
            .filter_map(|d| {
                let f = match d {
                    Decl::Func(f) => f,
                    _ => return None,
                };
                let body = f.body.as_ref()?;
                let mut sp = f.span.clone();
                remap_span(&mut sp, line_map);
                let mut end_sp = body.span.clone();
                remap_span(&mut end_sp, line_map);
                Some((f.name.name.clone(), sp.file.to_string(), sp.line, end_sp.line))
            })
            .collect();
        let triples: Vec<String> = if offload.target_triples.is_empty() {
            // 未指定 -fopenmp-targets: host fallback (device_triple 为空串)
            vec![String::new()]
        } else {
            offload.target_triples.clone()
        };
        for (pfile, pline, ptext) in omp_pragmas {
            let words: Vec<&str> = ptext.split_whitespace().collect();
            // 已知 offload 构造: target / target simd / simd;
            // 其它 OpenMP 构造 (parallel/atomic/task/...) 无 device
            // codegen, 警告后按 no-op 处理 (位置精确到 pragma 行)
            let kind = match words.as_slice() {
                ["target", "simd", ..] => Some(OffloadKind::OmpTargetSimd),
                ["target", ..] => Some(OffloadKind::OmpTarget),
                ["simd", ..] => Some(OffloadKind::OmpTargetSimd),
                _ => None,
            };
            let owner = funcs
                .iter()
                .find(|(_, file, start, end)| file == pfile && *start <= *pline && *end >= *pline);
            match (kind, owner) {
                (Some(k), Some((name, _, _, _))) => {
                    for t in &triples {
                        section.add_entry(OffloadEntry {
                            kind: k,
                            func: name.clone(),
                            // 位置指向 pragma 本身 (而非函数头)
                            file: pfile.clone(),
                            line: *pline,
                            col: 1,
                            device_triple: t.clone(),
                            arch: None,
                        });
                    }
                }
                (Some(_), None) => {
                    diags.push(Diagnostic::warn(
                        "W0311",
                        format!(
                            "-fopenmp: '#pragma omp {}' outside any function body; ignored",
                            words.join(" ")
                        ),
                        crate::diagnostics::Span::point(pfile, *pline, 1),
                    ));
                }
                (None, Some((name, _, _, _))) => {
                    diags.push(Diagnostic::warn(
                        "W0312",
                        format!(
                            "-fopenmp: unsupported OpenMP construct '#pragma omp {}' in \
                             function '{}'; treated as no-op (no device codegen)",
                            words.join(" "),
                            name
                        ),
                        // 位置指向 pragma 本身 (而非函数头)
                        crate::diagnostics::Span::point(pfile, *pline, 1),
                    ));
                }
                (None, None) => {
                    diags.push(Diagnostic::warn(
                        "W0313",
                        format!(
                            "-fopenmp: unsupported OpenMP construct '#pragma omp {}'; ignored",
                            words.join(" ")
                        ),
                        crate::diagnostics::Span::point(pfile, *pline, 1),
                    ));
                }
            }
        }
    }

    let out = if section.is_empty() { None } else { Some(section) };
    (out, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_dep_path_no_special() {
        assert_eq!(
            escape_dep_path("/usr/include/stdio.h"),
            "/usr/include/stdio.h"
        );
    }

    #[test]
    fn escape_dep_path_with_space() {
        assert_eq!(escape_dep_path("/my path/file.h"), "/my\\ path/file.h");
    }

    #[test]
    fn escape_dep_path_with_hash() {
        assert_eq!(escape_dep_path("/a#b.h"), "/a\\#b.h");
    }

    #[test]
    fn escape_dep_path_with_colon() {
        assert_eq!(escape_dep_path("/a:b.h"), "/a\\:b.h");
    }

    #[test]
    fn write_dep_file_basic_format() {
        let mut buf = Vec::new();
        write_dep_file(
            &mut buf,
            "foo.o",
            "foo.c",
            &["bar.h".to_string(), "baz.h".to_string()],
            false,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        // 第一行包含 target: source
        assert!(out.starts_with("foo.o: foo.c"));
        // 包含依赖
        assert!(out.contains("bar.h"));
        assert!(out.contains("baz.h"));
        // 使用 \ 续行符
        assert!(out.contains("\\\n"));
    }

    #[test]
    fn write_dep_file_phony_targets() {
        let mut buf = Vec::new();
        write_dep_file(&mut buf, "foo.o", "foo.c", &["bar.h".to_string()], true).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // -MP: 每个依赖应有空 phony 目标
        assert!(out.contains("\nbar.h:"));
    }

    #[test]
    fn write_dep_file_line_wrapping() {
        let mut buf = Vec::new();
        // 构造足够长的路径使行宽超过 72
        let long_dep = format!("{}{}", "/long/path/", "x".repeat(60));
        write_dep_file(
            &mut buf,
            "foo.o",
            "foo.c",
            std::slice::from_ref(&long_dep),
            false,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        // 应包含续行符
        assert!(out.contains("\\\n"));
    }

    #[test]
    fn write_dep_file_make_parseable() {
        // 验证输出可被 Make 正确解析: 所有依赖在同一规则中
        let mut buf = Vec::new();
        write_dep_file(
            &mut buf,
            "out.o",
            "src.c",
            &[
                "inc/a.h".to_string(),
                "inc/b.h".to_string(),
                "inc/c.h".to_string(),
            ],
            false,
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        // 所有依赖应通过 \ 连接, 不应出现裸新行分隔依赖
        let lines: Vec<&str> = out.lines().collect();
        // 除最后一行外, 每行应以 \ 结尾
        for line in &lines[..lines.len().saturating_sub(1)] {
            assert!(
                line.ends_with('\\'),
                "non-final line should end with \\: {line:?}"
            );
        }
    }

    #[test]
    fn quote_make_target_dollar_sign() {
        // -MQ: $ 应转义为 $$
        assert_eq!(quote_make_target("foo$bar.o"), "foo$$bar.o");
    }

    #[test]
    fn quote_make_target_no_special() {
        // 无特殊字符时保持不变
        assert_eq!(quote_make_target("foo.o"), "foo.o");
    }

    #[test]
    fn quote_make_target_space_and_colon() {
        // 空格和冒号用 \ 转义
        assert_eq!(quote_make_target("my obj.o"), "my\\ obj.o");
        assert_eq!(quote_make_target("a:b.o"), "a\\:b.o");
    }

    #[test]
    fn quote_make_target_multiple_dollars() {
        // 多个 $ 各自转义
        assert_eq!(quote_make_target("$a$b$"), "$$a$$b$$");
    }
}
